//! Fetching part of a commit: its trees first, then only the files under
//! the directories a dependency materializes.
//!
//! `gix` has no partial clone, so this speaks the fetch protocol through
//! `gix::protocol`: the wants are object hashes, there is nothing to
//! negotiate, and the trees are asked for with `filter blob:none`. A remote
//! that cannot filter is fetched whole instead.

use super::git::{failed, problem};
use cuenv_core::Result;
use gix::protocol::fetch::negotiate::{Action, Round, one_round};
use gix::protocol::fetch::{Arguments, Context, Negotiate, Options, Response, Shallow, Tags};
use gix::protocol::transport::Service;
use gix::remote::Direction;
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::atomic::AtomicBool;

/// The filter that leaves every file out of a fetch.
const WITHOUT_BLOBS: &str = "blob:none";

/// Whether a fetch brought what was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fetched {
    /// The objects are in the repository.
    Yes,
    /// The remote cannot leave files out of a fetch. Nothing was fetched.
    CannotFilter,
}

/// Which objects behind the wanted ones a fetch brings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// One commit and its trees, without the files they name.
    TreesOfOneCommit,
    /// The wanted objects, which are files.
    Files,
}

/// The wants of one fetch. Nothing is in common with the remote, so there
/// is nothing to negotiate.
struct Wants<'a> {
    ids: &'a [gix::ObjectId],
    scope: Scope,
    /// Set when the remote cannot apply the filter the scope needs.
    cannot_filter: bool,
}

impl Negotiate for Wants<'_> {
    fn mark_complete_and_common_ref(
        &mut self,
    ) -> std::result::Result<Action, gix::protocol::fetch::negotiate::Error> {
        Ok(Action::MustNegotiate {
            remote_ref_target_known: Vec::new(),
        })
    }

    fn add_wants(&mut self, arguments: &mut Arguments, _remote_ref_target_known: &[bool]) -> bool {
        if self.scope == Scope::TreesOfOneCommit {
            if !arguments.can_use_filter() {
                self.cannot_filter = true;
                return false;
            }
            arguments.filter(WITHOUT_BLOBS);
        }
        for id in self.ids {
            arguments.want(id);
        }
        true
    }

    fn one_round(
        &mut self,
        _state: &mut one_round::State,
        _arguments: &mut Arguments,
        _previous_response: Option<&Response>,
    ) -> std::result::Result<(Round, bool), gix::protocol::fetch::negotiate::Error> {
        let nothing_in_common = Round {
            haves_sent: 0,
            in_vain: 0,
            haves_to_send: 0,
            previous_response_had_at_least_one_in_common: false,
        };
        Ok((nothing_in_common, true))
    }
}

/// Fetch the trees of one commit of a remote, without its files.
pub(super) fn fetch_trees(
    repository: &gix::Repository,
    url: &str,
    commit: &str,
) -> Result<Fetched> {
    let commit = gix::ObjectId::from_hex(commit.as_bytes()).map_err(failed)?;
    fetch_objects(
        repository,
        Request {
            url,
            ids: &[commit],
            scope: Scope::TreesOfOneCommit,
        },
    )
}

/// Fetch the files of a tree that the repository does not have yet. A
/// submodule is not a file of the tree.
pub(super) fn fetch_files(repository: &gix::Repository, url: &str, tree: &str) -> Result<()> {
    let tree = gix::ObjectId::from_hex(tree.as_bytes()).map_err(failed)?;
    let index = repository.index_from_tree(&tree).map_err(failed)?;
    let missing: BTreeSet<gix::ObjectId> = index
        .entries()
        .iter()
        .filter(|entry| entry.mode != gix::index::entry::Mode::COMMIT)
        .map(|entry| entry.id)
        .filter(|id| !repository.has_object(id))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let missing: Vec<gix::ObjectId> = missing.into_iter().collect();
    fetch_objects(
        repository,
        Request {
            url,
            ids: &missing,
            scope: Scope::Files,
        },
    )?;
    match missing.iter().find(|id| !repository.has_object(id)) {
        Some(id) => Err(problem(format!("'{url}' did not send the file {id}"))),
        None => Ok(()),
    }
}

/// The objects to ask a remote for.
#[derive(Debug, Clone, Copy)]
struct Request<'a> {
    url: &'a str,
    ids: &'a [gix::ObjectId],
    scope: Scope,
}

/// Ask a remote for objects by hash and store the pack it answers with.
fn fetch_objects(repository: &gix::Repository, request: Request<'_>) -> Result<Fetched> {
    let Request { url, ids, scope } = request;
    let remote = repository.remote_at(url).map_err(failed)?;
    let mut connection = remote.connect(Direction::Fetch).map_err(failed)?;
    let remote_url = connection
        .remote()
        .url(Direction::Fetch)
        .cloned()
        .ok_or_else(|| problem(format!("'{url}' is not a remote to fetch from")))?;
    let credentials = connection
        .configured_credentials(remote_url)
        .map_err(failed)?;
    if let Some(options) = repository.transport_options(url, None).map_err(failed)? {
        connection
            .transport_mut()
            .configure(&*options)
            .map_err(|error| problem(format!("could not configure the transport: {error}")))?;
    }
    let mut handshake = gix::protocol::handshake(
        connection.transport_mut(),
        Service::UploadPack,
        credentials,
        Vec::new(),
        &mut gix::progress::Discard,
    )
    .map_err(failed)?;

    let shallow = match scope {
        Scope::TreesOfOneCommit => Shallow::DepthAtRemote(NonZeroU32::MIN),
        Scope::Files => Shallow::NoChange,
    };
    let mut wants = Wants {
        ids,
        scope,
        cannot_filter: false,
    };
    let pack_directory = repository.objects.store_ref().path().join("pack");
    let pack_options = gix::odb::pack::bundle::write::Options {
        thread_limit: None,
        index_version: gix::odb::pack::index::Version::default(),
        iteration_mode: gix::odb::pack::data::input::Mode::Verify,
        object_hash: repository.object_hash(),
    };
    let protocol = handshake.server_protocol_version;
    let outcome = gix::protocol::fetch(
        &mut wants,
        |pack, progress, should_interrupt| {
            let bundle = gix::odb::pack::Bundle::write_to_directory(
                pack,
                Some(&pack_directory),
                progress,
                should_interrupt,
                Some(repository.objects.clone()),
                pack_options,
            )?;
            // The pack is not kept from a repack: the repository is scratch.
            if let Some(keep) = bundle.keep_path {
                std::fs::remove_file(keep).ok();
            }
            Ok::<_, gix::odb::pack::bundle::write::Error>(true)
        },
        gix::progress::Discard,
        &AtomicBool::new(false),
        Context {
            handshake: &mut handshake,
            transport: connection.transport_mut(),
            user_agent: (
                "agent",
                Some(Cow::Owned(gix::protocol::agent(gix::env::agent()))),
            ),
            trace_packetlines: false,
        },
        Options {
            shallow_file: repository.shallow_file(),
            shallow: &shallow,
            tags: Tags::None,
            reject_shallow_remote: false,
        },
    )
    .map_err(failed)?;
    if protocol == gix::protocol::transport::Protocol::V2 {
        gix::protocol::indicate_end_of_interaction(connection.transport_mut(), false).ok();
    }

    if wants.cannot_filter {
        return Ok(Fetched::CannotFilter);
    }
    if outcome.is_none() {
        return Err(problem(format!("'{url}' sent nothing for {ids:?}")));
    }
    Ok(Fetched::Yes)
}
