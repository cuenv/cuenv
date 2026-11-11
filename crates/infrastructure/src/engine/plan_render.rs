//! The plan a person reads: one structural diff per change.
//!
//! Nested values are laid out over several lines, updates show only what
//! differs, and a string holding a JSON document is compared as that
//! document. The reviewed plan digest is not computed from this text; it
//! keeps its own compact rendering so that a change to the layout here
//! never changes a digest.

use std::collections::{BTreeMap, BTreeSet};

use super::infrastructure_engine::sensitive_reference_paths;
use super::{Action, Plan, ResourceChange};
use crate::type_system::Value;

/// Columns before an attribute's change marker.
const ATTRIBUTE_INDENT: usize = 6;
/// Columns before a resource's change marker.
const RESOURCE_INDENT: usize = 2;
/// Columns per level of nesting.
const NESTING: usize = 4;
/// Widest line a value is kept on before it is laid out over several.
const WIDTH: usize = 100;
/// Most attribute names the overview lists for one change.
const LISTED_ATTRIBUTES: usize = 6;
/// Largest list comparison aligned element by element.
const ALIGNMENT_LIMIT: usize = 250_000;
/// Score of two equal list elements; similar ones score below it.
const EQUAL: u32 = 1000;
/// Lowest score at which two differing elements are shown as one change.
const SIMILAR: u32 = 500;

/// How [`render_plan_styled`] marks additions, removals and changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlanStyle {
    /// Markers only.
    #[default]
    Plain,
    /// Markers and ANSI colors, for a terminal.
    Ansi,
}

/// Render a plan as human-readable text without colors.
#[must_use]
pub fn render_plan(plan: &Plan) -> String {
    render_plan_styled(plan, PlanStyle::Plain)
}

/// Render a plan as human-readable text: a table of every resource and
/// what happens to it, the changes in detail, and the totals.
#[must_use]
pub fn render_plan_styled(plan: &Plan, style: PlanStyle) -> String {
    let mut page = Page::default();
    overview(&mut page, plan);
    details(&mut page, plan);
    totals(&mut page, plan);
    page.render(style)
}

/// One row of the overview table.
struct Entry {
    tone: Tone,
    marker: &'static str,
    kind: String,
    name: String,
    plan: &'static str,
    info: String,
}

impl Entry {
    fn of(change: &ResourceChange) -> Option<Self> {
        let (tone, plan, info) = match change.action {
            Action::NoOp => return None,
            Action::Refresh => (Tone::Context, "refresh", "stored state only".to_string()),
            Action::Create => (Tone::Add, "create", String::new()),
            Action::Update => (Tone::Change, "update", changed_attributes(change)),
            Action::Replace => (Tone::Change, "replace", changed_attributes(change)),
            Action::Delete => (Tone::Remove, "delete", String::new()),
        };
        Some(Self {
            tone,
            marker: change.action.symbol(),
            kind: change.address.resource_type.clone(),
            name: change.address.name.clone(),
            plan,
            info,
        })
    }
}

/// The attributes an update or replacement touches, as `[diff: ~a,+b,-c]`.
fn changed_attributes(change: &ResourceChange) -> String {
    let empty = BTreeMap::new();
    let before = match change.before.untyped() {
        Value::Object(attributes) => attributes,
        _ => &empty,
    };
    let after = match change.after.untyped() {
        Value::Object(attributes) => attributes,
        _ => &empty,
    };
    let names: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let changed: Vec<String> = names
        .into_iter()
        .filter_map(|name| {
            let old = before.get(name).unwrap_or(&Value::Null);
            let new = after.get(name).unwrap_or(&Value::Null);
            let sign = match (old.is_null(), new.is_null()) {
                _ if old == new => return None,
                (true, _) => '+',
                (_, true) => '-',
                _ => '~',
            };
            Some(format!("{sign}{name}"))
        })
        .collect();
    match changed.len() {
        0 => String::new(),
        count if count > LISTED_ATTRIBUTES => format!(
            "[diff: {},… {} more]",
            changed[..LISTED_ATTRIBUTES].join(","),
            count - LISTED_ATTRIBUTES
        ),
        _ => format!("[diff: {}]", changed.join(",")),
    }
}

/// The table: every data source read and every resource with work.
fn overview(page: &mut Page, plan: &Plan) {
    let entries: Vec<Entry> = plan
        .data_sources
        .iter()
        .map(|read| Entry {
            tone: Tone::Context,
            marker: "<=",
            kind: format!("data.{}", read.address.resource_type),
            name: read.address.name.clone(),
            plan: if read.deferred {
                "read during apply"
            } else {
                "read"
            },
            info: String::new(),
        })
        .chain(plan.changes.iter().filter_map(Entry::of))
        .collect();
    if entries.is_empty() {
        return;
    }
    let width = |header: &str, column: fn(&Entry) -> &str| {
        entries
            .iter()
            .map(|entry| column(entry).chars().count())
            .chain([header.chars().count()])
            .max()
            .unwrap_or(0)
    };
    let kinds = width("Type", |entry| &entry.kind);
    let names = width("Name", |entry| &entry.name);
    let plans = width("Plan", |entry| entry.plan);
    let row = |kind: &str, name: &str, plan: &str, info: &str| {
        format!("{kind:<kinds$}  {name:<names$}  {plan:<plans$}  {info}")
            .trim_end()
            .to_string()
    };
    page.title(format!("     {}", row("Type", "Name", "Plan", "Info")));
    for entry in &entries {
        page.lines.push(Line {
            tone: entry.tone,
            indent: 1,
            marker: format!("{:<3}", entry.marker),
            depth: 0,
            text: row(&entry.kind, &entry.name, entry.plan, &entry.info),
        });
    }
}

/// Each change to real infrastructure as a diff.
fn details(page: &mut Page, plan: &Plan) {
    let mut changes = Page::default();
    for read in &plan.data_sources {
        let paths = sensitive_reference_paths(&read.references, &read.sensitive);
        if paths.is_empty() {
            continue;
        }
        changes.blank();
        changes.heading(Tone::Context, "<=", format!("data.{}", read.address));
        for path in paths {
            changes.remark(format!("{path} = (sensitive)"));
        }
    }
    for change in &plan.changes {
        resource(&mut changes, change);
    }
    if changes.lines.is_empty() {
        return;
    }
    page.blank();
    page.title("Changes:".to_string());
    page.lines.append(&mut changes.lines);
}

/// How many resources each action touches; actions touching none are left out.
fn totals(page: &mut Page, plan: &Plan) {
    let summary = plan.summary();
    let counts = [
        (Tone::Add, "+", summary.create, "to create"),
        (Tone::Change, "~", summary.update, "to update"),
        (Tone::Change, "-/+", summary.replace, "to replace"),
        (Tone::Remove, "-", summary.delete, "to delete"),
        (Tone::Context, "", summary.refresh, "to refresh"),
        (Tone::Context, "", summary.unchanged, "unchanged"),
    ];
    // Only a replacement's marker is wider than one column.
    let width = if summary.replace > 0 { 3 } else { 1 };
    page.blank();
    page.title("Resources:".to_string());
    for (tone, marker, count, what) in counts {
        if count > 0 {
            page.lines.push(Line {
                tone,
                indent: 4,
                marker: format!("{marker:<width$}"),
                depth: 0,
                text: format!("{count} {what}"),
            });
        }
    }
}

fn resource(page: &mut Page, change: &ResourceChange) {
    let tone = match change.action {
        Action::NoOp | Action::Refresh => return,
        Action::Create => Tone::Add,
        Action::Update | Action::Replace => Tone::Change,
        Action::Delete => Tone::Remove,
    };
    page.blank();
    page.heading(tone, change.action.symbol(), change.address.to_string());
    if !change.requires_replace.is_empty() {
        page.remark(format!(
            "replacement forced by: {}",
            change.requires_replace.join(", ")
        ));
    }
    attributes(page, change);
    for argument in change
        .references
        .secrets()
        .iter()
        .filter(|argument| argument.write_only.is_some())
    {
        page.line(
            Tone::Context,
            0,
            format!("{} = (write-only)", argument.destination),
        );
    }
    for path in sensitive_reference_paths(&change.references, &change.sensitive) {
        page.remark(format!("{path} = (sensitive)"));
    }
}

fn attributes(page: &mut Page, change: &ResourceChange) {
    let empty = BTreeMap::new();
    let before = match change.before.untyped() {
        Value::Object(attributes) => attributes,
        _ => &empty,
    };
    let after = match change.after.untyped() {
        Value::Object(attributes) => attributes,
        _ => &empty,
    };
    let referenced: BTreeSet<&str> = change
        .references
        .references()
        .iter()
        .map(|reference| reference.destination_attribute.as_str())
        .collect();
    let mut computed = Vec::new();
    let names: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for name in names {
        let attribute = Attribute {
            name,
            before: before.get(name).unwrap_or(&Value::Null),
            after: after.get(name).unwrap_or(&Value::Null),
            masked: change.sensitive.contains(name),
            unknowns: if referenced.contains(name.as_str()) {
                Unknowns::Shown
            } else {
                Unknowns::Hidden
            },
        };
        match change.action {
            Action::Create
                if attribute.after.is_unknown() && !attribute.masked && !attribute.referenced() =>
            {
                computed.push(name.as_str());
            }
            Action::Create if !attribute.after.is_null() => {
                attribute.whole(page, Tone::Add, attribute.after);
            }
            Action::Delete if !attribute.before.is_null() => {
                attribute.whole(page, Tone::Remove, attribute.before);
            }
            Action::Update | Action::Replace if attribute.after != attribute.before => {
                attribute.difference(page);
            }
            _ => {}
        }
    }
    if !computed.is_empty() {
        page.remark(format!("known after apply: {}", computed.join(", ")));
    }
}

/// Whether unknown attributes nested in an added or removed value are
/// listed or only counted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unknowns {
    /// A reference fills the attribute, so what is still unknown matters.
    Shown,
    /// The provider computes them; they say nothing a reviewer can check.
    Hidden,
}

/// One top-level attribute of a change.
struct Attribute<'change> {
    name: &'change str,
    before: &'change Value,
    after: &'change Value,
    masked: bool,
    unknowns: Unknowns,
}

impl Attribute<'_> {
    fn referenced(&self) -> bool {
        self.unknowns == Unknowns::Shown
    }

    fn slot(&self, tone: Tone) -> Slot {
        Slot {
            tone,
            depth: 0,
            prefix: format!("{} = ", self.name),
            suffix: String::new(),
        }
    }

    /// The value as the compact text plans have always shown; sensitive
    /// values are masked.
    fn compact(&self, value: &Value) -> String {
        if self.masked && !value.is_null() {
            "(sensitive)".to_string()
        } else {
            value.to_string()
        }
    }

    /// The value laid out structurally. `None` when it is masked, or when
    /// redaction would rewrite its compact text: a registered secret must
    /// stay on one line for redaction to find it.
    fn structured(&self, value: &Value) -> Option<Shown> {
        let compact = value.to_string();
        (!self.masked && cuenv_events::redact_cow(&compact) == compact).then(|| Shown::of(value))
    }

    /// An attribute that is wholly added or removed.
    fn whole(&self, page: &mut Page, tone: Tone, value: &Value) {
        match self.structured(value) {
            Some(shown) => Layout {
                page,
                unknowns: self.unknowns,
            }
            .value(&self.slot(tone), &shown),
            None => page.line(tone, 0, format!("{} = {}", self.name, self.compact(value))),
        }
    }

    /// An attribute whose value changes.
    fn difference(&self, page: &mut Page) {
        let shown = self
            .structured(self.before)
            .zip(self.structured(self.after))
            // Values that differ only in what is not displayed (the spacing
            // of a JSON document, a number's spelling) have no structural
            // difference to show.
            .filter(|(before, after)| before != after);
        match shown {
            Some((before, after)) => Layout {
                page,
                unknowns: self.unknowns,
            }
            .difference(&self.slot(Tone::Change), &before, &after),
            None => page.line(
                Tone::Change,
                0,
                format!(
                    "{} = {} -> {}",
                    self.name,
                    self.compact(self.before),
                    self.compact(self.after)
                ),
            ),
        }
    }
}

/// A value as it is displayed.
#[derive(Debug, Clone, PartialEq)]
enum Shown {
    Null,
    Unknown,
    /// A boolean, number or quoted string, already rendered.
    Scalar(String),
    List(Vec<Self>),
    Object(Vec<(String, Self)>),
    /// A string holding a JSON document, shown as that document.
    Encoded(Box<Self>),
}

impl Shown {
    fn of(value: &Value) -> Self {
        match value.untyped() {
            Value::Null => Self::Null,
            Value::Unknown => Self::Unknown,
            Value::Boolean(boolean) => Self::Scalar(boolean.to_string()),
            Value::Number(number) => Self::Scalar(number.to_string()),
            Value::String(text) => Self::of_string(text),
            Value::List(items) => Self::List(items.iter().map(Self::of).collect()),
            Value::Object(attributes) => Self::Object(
                attributes
                    .iter()
                    .map(|(name, value)| (name.clone(), Self::of(value)))
                    .collect(),
            ),
            Value::Typed(typed) => Self::of(&typed.value),
        }
    }

    fn of_string(text: &str) -> Self {
        let document = text
            .trim_start()
            .starts_with(['{', '['])
            .then(|| serde_json::from_str::<serde_json::Value>(text).ok())
            .flatten()
            .filter(|json| json.is_object() || json.is_array());
        document.map_or_else(
            || Self::Scalar(serde_json::Value::from(text).to_string()),
            |json| Self::Encoded(Box::new(Self::of_json(&json))),
        )
    }

    fn of_json(json: &serde_json::Value) -> Self {
        use serde_json::Value as Json;
        match json {
            Json::Null => Self::Null,
            Json::Bool(_) | Json::Number(_) | Json::String(_) => Self::Scalar(json.to_string()),
            Json::Array(items) => Self::List(items.iter().map(Self::of_json).collect()),
            Json::Object(fields) => Self::Object(
                fields
                    .iter()
                    .map(|(name, value)| (name.clone(), Self::of_json(value)))
                    .collect(),
            ),
        }
    }

    /// Null, or a collection holding nothing.
    fn is_unset(&self) -> bool {
        match self {
            Self::Null => true,
            Self::List(items) => items.is_empty(),
            Self::Object(fields) => fields.is_empty(),
            Self::Unknown | Self::Scalar(_) | Self::Encoded(_) => false,
        }
    }

    const fn is_leaf(&self) -> bool {
        matches!(self, Self::Null | Self::Unknown | Self::Scalar(_))
    }

    fn fields(&self) -> Option<&[(String, Self)]> {
        match self {
            Self::Object(fields) => Some(fields),
            Self::Encoded(inner) => inner.fields(),
            _ => None,
        }
    }

    /// How alike two list elements are, out of [`EQUAL`]. Elements scoring
    /// under [`SIMILAR`] are unrelated: one is removed, the other added.
    fn likeness(&self, other: &Self) -> u32 {
        if self == other {
            return EQUAL;
        }
        let (Some(left), Some(right)) = (self.fields(), other.fields()) else {
            return 0;
        };
        let names: BTreeSet<&str> = left
            .iter()
            .chain(right)
            .map(|(name, _)| name.as_str())
            .collect();
        let same = names
            .iter()
            .filter(|name| field(left, name).is_some() && field(left, name) == field(right, name))
            .count();
        let score = u32::try_from(same * 1000 / names.len().max(1)).unwrap_or(0);
        if score < SIMILAR {
            0
        } else {
            score.min(EQUAL - 1)
        }
    }
}

fn field<'shown>(fields: &'shown [(String, Shown)], name: &str) -> Option<&'shown Shown> {
    fields
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| value)
}

/// A field name, quoted unless it is a plain identifier.
fn label(name: &str) -> String {
    let plain = name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
        && !name.starts_with(|character: char| character.is_ascii_digit())
        && !name.is_empty();
    if plain {
        name.to_string()
    } else {
        serde_json::Value::from(name).to_string()
    }
}

/// Where a value goes: its marker, nesting, and the text around it.
struct Slot {
    tone: Tone,
    depth: usize,
    prefix: String,
    suffix: String,
}

impl Slot {
    fn fits(&self, text: &str) -> bool {
        ATTRIBUTE_INDENT
            + 2
            + self.depth * NESTING
            + self.prefix.chars().count()
            + text.chars().count()
            + self.suffix.chars().count()
            <= WIDTH
    }

    fn text(&self, text: &str) -> String {
        format!("{}{text}{}", self.prefix, self.suffix)
    }

    fn element(&self, tone: Tone) -> Self {
        Self {
            tone,
            depth: self.depth + 1,
            prefix: String::new(),
            suffix: ",".to_string(),
        }
    }

    fn field(&self, tone: Tone, name: &str) -> Self {
        Self {
            tone,
            depth: self.depth + 1,
            prefix: format!("{}: ", label(name)),
            suffix: String::new(),
        }
    }

    fn encoded(&self) -> Self {
        Self {
            tone: self.tone,
            depth: self.depth,
            prefix: format!("{}jsonencode(", self.prefix),
            suffix: format!("){}", self.suffix),
        }
    }
}

/// One step of turning the old list into the new one.
enum Step<'shown> {
    Keep(&'shown Shown),
    Remove(&'shown Shown),
    Add(&'shown Shown),
    Change(&'shown Shown, &'shown Shown),
}

/// Pair the elements of two lists so that the pairs are as alike as
/// possible while both lists keep their order.
fn align<'shown>(before: &'shown [Shown], after: &'shown [Shown]) -> Vec<Step<'shown>> {
    let unrelated = || {
        before
            .iter()
            .map(Step::Remove)
            .chain(after.iter().map(Step::Add))
            .collect()
    };
    if before.len().saturating_mul(after.len()) > ALIGNMENT_LIMIT {
        return unrelated();
    }
    let columns = after.len() + 1;
    let cell = |row: usize, column: usize| row * columns + column;
    // best[cell(row, column)]: the best total for before[row..] against
    // after[column..].
    let mut best = vec![0_u32; (before.len() + 1) * columns];
    for row in (0..before.len()).rev() {
        for column in (0..after.len()).rev() {
            let paired = match before[row].likeness(&after[column]) {
                0 => 0,
                score => score + best[cell(row + 1, column + 1)],
            };
            best[cell(row, column)] = paired
                .max(best[cell(row + 1, column)])
                .max(best[cell(row, column + 1)]);
        }
    }
    let mut steps = Vec::with_capacity(before.len().max(after.len()));
    let (mut row, mut column) = (0, 0);
    while row < before.len() && column < after.len() {
        let score = before[row].likeness(&after[column]);
        if score > 0 && best[cell(row, column)] == score + best[cell(row + 1, column + 1)] {
            steps.push(if score == EQUAL {
                Step::Keep(&after[column])
            } else {
                Step::Change(&before[row], &after[column])
            });
            row += 1;
            column += 1;
        } else if best[cell(row, column)] == best[cell(row + 1, column)] {
            steps.push(Step::Remove(&before[row]));
            row += 1;
        } else {
            steps.push(Step::Add(&after[column]));
            column += 1;
        }
    }
    steps.extend(before[row..].iter().map(Step::Remove));
    steps.extend(after[column..].iter().map(Step::Add));
    steps
}

/// Lays values and their differences out on a page.
struct Layout<'page> {
    page: &'page mut Page,
    unknowns: Unknowns,
}

impl Layout<'_> {
    /// Whether a field of an added or removed object is left out.
    fn omits(&self, shown: &Shown) -> bool {
        shown.is_unset() || (self.unknowns == Unknowns::Hidden && *shown == Shown::Unknown)
    }

    /// The value on one line, unless it holds a field that is left out:
    /// such an object is laid out so the omission can be noted.
    fn inline(&self, shown: &Shown) -> Option<String> {
        match shown {
            Shown::Null => Some("null".to_string()),
            Shown::Unknown => Some("(known after apply)".to_string()),
            Shown::Scalar(text) => Some(text.clone()),
            Shown::List(items) => items
                .iter()
                .map(|item| self.inline(item))
                .collect::<Option<Vec<_>>>()
                .map(|items| format!("[{}]", items.join(", "))),
            Shown::Object(fields) => fields
                .iter()
                .map(|(name, value)| {
                    (!self.omits(value))
                        .then(|| self.inline(value))
                        .flatten()
                        .map(|value| format!("{}: {value}", label(name)))
                })
                .collect::<Option<Vec<_>>>()
                .map(|fields| format!("{{{}}}", fields.join(", "))),
            Shown::Encoded(inner) => self
                .inline(inner)
                .map(|inner| format!("jsonencode({inner})")),
        }
    }

    /// A value that is wholly added, removed or unchanged.
    fn value(&mut self, slot: &Slot, shown: &Shown) {
        let inline = self.inline(shown);
        if let Some(text) = inline.as_deref().filter(|text| slot.fits(text)) {
            self.page.line(slot.tone, slot.depth, slot.text(text));
            return;
        }
        match shown {
            Shown::List(items) => {
                self.page.line(slot.tone, slot.depth, slot.text_open("["));
                for item in items {
                    self.value(&slot.element(slot.tone), item);
                }
                self.page.line(slot.tone, slot.depth, slot.text_close("]"));
            }
            Shown::Object(fields) => {
                self.page.line(slot.tone, slot.depth, slot.text_open("{"));
                let (mut unset, mut unknown) = (0_usize, 0_usize);
                for (name, value) in fields {
                    if value.is_unset() {
                        unset += 1;
                    } else if self.omits(value) {
                        unknown += 1;
                    } else {
                        self.value(&slot.field(slot.tone, name), value);
                    }
                }
                let omitted: Vec<String> = [(unknown, "known after apply"), (unset, "unset")]
                    .into_iter()
                    .filter(|(count, _)| *count > 0)
                    .map(|(count, kind)| format!("{count} {kind}"))
                    .collect();
                if !omitted.is_empty() {
                    self.page.line(
                        Tone::Note,
                        slot.depth + 1,
                        format!("# {} hidden", omitted.join(", ")),
                    );
                }
                self.page.line(slot.tone, slot.depth, slot.text_close("}"));
            }
            Shown::Encoded(inner) => self.value(&slot.encoded(), inner),
            Shown::Null | Shown::Unknown | Shown::Scalar(_) => {
                self.page.line(
                    slot.tone,
                    slot.depth,
                    slot.text(inline.as_deref().unwrap_or_default()),
                );
            }
        }
    }

    /// Two values that differ.
    fn difference(&mut self, slot: &Slot, before: &Shown, after: &Shown) {
        // A change small enough for one line reads best as `old -> new`.
        let arrow = (!matches!(before, Shown::Null) && !matches!(after, Shown::Null))
            .then(|| self.inline(before).zip(self.inline(after)))
            .flatten()
            .map(|(old, new)| format!("{old} -> {new}"))
            .filter(|arrow| slot.fits(arrow));
        if let Some(arrow) = arrow {
            self.page.line(Tone::Change, slot.depth, slot.text(&arrow));
            return;
        }
        match (before, after) {
            (Shown::Object(old), Shown::Object(new)) => {
                self.page
                    .line(Tone::Change, slot.depth, slot.text_open("{"));
                let names: BTreeSet<&str> = old
                    .iter()
                    .chain(new)
                    .map(|(name, _)| name.as_str())
                    .collect();
                let mut unchanged = 0_usize;
                for name in names {
                    match (field(old, name), field(new, name)) {
                        (Some(old), Some(new)) if old == new => unchanged += 1,
                        (Some(old), Some(new)) => {
                            self.difference(&slot.field(Tone::Change, name), old, new);
                        }
                        (Some(old), None) => self.value(&slot.field(Tone::Remove, name), old),
                        (None, Some(new)) => self.value(&slot.field(Tone::Add, name), new),
                        (None, None) => {}
                    }
                }
                self.hidden(slot.depth + 1, unchanged, "attribute");
                self.page
                    .line(Tone::Context, slot.depth, slot.text_close("}"));
            }
            (Shown::List(old), Shown::List(new)) => {
                self.page
                    .line(Tone::Change, slot.depth, slot.text_open("["));
                let mut unchanged = 0_usize;
                for step in align(old, new) {
                    if let Step::Keep(kept) = step {
                        if kept.is_leaf() {
                            self.value(&slot.element(Tone::Context), kept);
                        } else {
                            unchanged += 1;
                        }
                        continue;
                    }
                    self.hidden(slot.depth + 1, unchanged, "element");
                    unchanged = 0;
                    match step {
                        Step::Remove(old) => self.value(&slot.element(Tone::Remove), old),
                        Step::Add(new) => self.value(&slot.element(Tone::Add), new),
                        Step::Change(old, new) => {
                            self.difference(&slot.element(Tone::Change), old, new);
                        }
                        Step::Keep(_) => {}
                    }
                }
                self.hidden(slot.depth + 1, unchanged, "element");
                self.page
                    .line(Tone::Context, slot.depth, slot.text_close("]"));
            }
            (Shown::Encoded(old), Shown::Encoded(new)) => {
                self.difference(&slot.encoded(), old, new);
            }
            (Shown::Null, new) => self.value(&slot.toned(Tone::Add), new),
            (old, Shown::Null) => self.value(&slot.toned(Tone::Remove), old),
            (old, new) => {
                self.value(&slot.toned(Tone::Remove), old);
                self.value(&slot.toned(Tone::Add), new);
            }
        }
    }

    fn hidden(&mut self, depth: usize, count: usize, kind: &str) {
        if count > 0 {
            let plural = if count == 1 { "" } else { "s" };
            self.page.line(
                Tone::Note,
                depth,
                format!("# {count} unchanged {kind}{plural} hidden"),
            );
        }
    }
}

impl Slot {
    fn text_open(&self, bracket: &str) -> String {
        format!("{}{bracket}", self.prefix)
    }

    fn text_close(&self, bracket: &str) -> String {
        format!("{bracket}{}", self.suffix)
    }

    fn toned(&self, tone: Tone) -> Self {
        Self {
            tone,
            depth: self.depth,
            prefix: self.prefix.clone(),
            suffix: self.suffix.clone(),
        }
    }
}

/// What a line says about the plan; it picks the marker and the color.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Add,
    Remove,
    Change,
    Context,
    Note,
    /// A section or table heading.
    Title,
    /// An empty line.
    Blank,
}

impl Tone {
    const fn marker(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Remove => "-",
            Self::Change => "~",
            Self::Context | Self::Note | Self::Title | Self::Blank => " ",
        }
    }
}

struct Line {
    tone: Tone,
    indent: usize,
    marker: String,
    depth: usize,
    text: String,
}

/// The lines of a plan, in order.
#[derive(Default)]
struct Page {
    lines: Vec<Line>,
}

impl Page {
    fn blank(&mut self) {
        self.title(String::new());
        if let Some(line) = self.lines.last_mut() {
            line.tone = Tone::Blank;
        }
    }

    fn title(&mut self, text: String) {
        self.lines.push(Line {
            tone: Tone::Title,
            indent: 0,
            marker: String::new(),
            depth: 0,
            text,
        });
    }

    /// The line naming a data source or resource above its changes.
    fn heading(&mut self, tone: Tone, marker: &str, text: String) {
        self.lines.push(Line {
            tone,
            indent: RESOURCE_INDENT,
            marker: marker.to_string(),
            depth: 0,
            text,
        });
    }

    /// A comment about the resource above.
    fn remark(&mut self, text: String) {
        self.lines.push(Line {
            tone: Tone::Note,
            indent: ATTRIBUTE_INDENT,
            marker: "#".to_string(),
            depth: 0,
            text,
        });
    }

    /// A line of an attribute's value, `depth` levels into it.
    fn line(&mut self, tone: Tone, depth: usize, text: String) {
        self.lines.push(Line {
            tone,
            indent: ATTRIBUTE_INDENT,
            marker: tone.marker().to_string(),
            depth,
            text,
        });
    }

    fn render(&self, style: PlanStyle) -> String {
        self.lines
            .iter()
            .map(|line| {
                let indent = " ".repeat(line.indent);
                let body = format!("{}{}", " ".repeat(line.depth * NESTING), line.text);
                let marker = &line.marker;
                match (style, line.tone) {
                    (_, Tone::Blank) => "\n".to_string(),
                    (PlanStyle::Plain, Tone::Title) => format!("{body}\n"),
                    (PlanStyle::Ansi, Tone::Title) => format!("\u{1b}[1m{body}\u{1b}[0m\n"),
                    (PlanStyle::Plain, _) | (PlanStyle::Ansi, Tone::Context) => {
                        format!("{indent}{marker} {body}\n")
                    }
                    (PlanStyle::Ansi, Tone::Add) => {
                        format!("{indent}\u{1b}[32m{marker} {body}\u{1b}[0m\n")
                    }
                    (PlanStyle::Ansi, Tone::Remove) => {
                        format!("{indent}\u{1b}[31m{marker} {body}\u{1b}[0m\n")
                    }
                    (PlanStyle::Ansi, Tone::Change) => {
                        format!("{indent}\u{1b}[33m{marker}\u{1b}[0m {body}\n")
                    }
                    (PlanStyle::Ansi, Tone::Note) => {
                        format!("{indent}\u{1b}[2m{marker} {body}\u{1b}[0m\n")
                    }
                }
            })
            .collect()
    }
}
