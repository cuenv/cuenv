use super::digest::Digest;
use crate::reapi::build::bazel::remote::execution::v2 as reapi;
use cuenv_core::tasks::io::ResolvedInputs;
use prost::Message;
use std::collections::{BTreeMap, HashMap};
use std::path::Component;

#[derive(Debug, PartialEq)]
pub struct MerkleTree {
    pub root_digest: Digest,
    pub directories: HashMap<Digest, Directory>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Directory {
    pub files: Vec<FileNode>,
    pub directories: Vec<DirectoryNode>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct FileNode {
    pub name: String,
    pub digest: Digest,
    pub is_executable: bool,
}

#[derive(Debug, PartialEq, Clone)]
pub struct DirectoryNode {
    pub name: String,
    pub digest: Digest,
}

impl Directory {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            directories: Vec::new(),
        }
    }

    pub fn to_proto(&self) -> reapi::Directory {
        reapi::Directory {
            files: self.files.iter().map(|f| reapi::FileNode {
                name: f.name.clone(),
                digest: Some(reapi::Digest {
                    hash: f.digest.hash.clone(),
                    size_bytes: f.digest.size_bytes,
                }),
                is_executable: f.is_executable,
                node_properties: None,
            }).collect(),
            directories: self.directories.iter().map(|d| reapi::DirectoryNode {
                name: d.name.clone(),
                digest: Some(reapi::Digest {
                    hash: d.digest.hash.clone(),
                    size_bytes: d.digest.size_bytes,
                }),
            }).collect(),
            symlinks: vec![], // TODO: support symlinks
            node_properties: None,
        }
    }

    pub fn digest(&self) -> Digest {
        let proto = self.to_proto();
        let mut buf = Vec::new();
        proto.encode(&mut buf).expect("failed to encode directory");
        Digest::from_content(&buf)
    }
}

impl MerkleTree {
    pub fn from_inputs(inputs: &ResolvedInputs) -> Result<Self, crate::RemoteError> {
        let mut files_by_dir: BTreeMap<Vec<String>, Vec<FileNode>> = BTreeMap::new();

        // 1. Group files by their parent directory components
        for input in &inputs.files {
            let mut components: Vec<String> = input.rel_path
                .components()
                .filter_map(|c| match c {
                    Component::Normal(s) => Some(s.to_string_lossy().to_string()),
                    _ => None,
                })
                .collect();
            
            if let Some(file_name) = components.pop() {
                let file_node = FileNode {
                    name: file_name,
                    digest: Digest::new(input.sha256.clone(), input.size as i64),
                    // TODO: check is_executable from file metadata or inputs
                    is_executable: false, 
                };
                files_by_dir.entry(components).or_default().push(file_node);
            }
        }

        // 2. Build directories bottom-up
        let mut directories = HashMap::new();
        // We iterate from longest paths (deepest) to shortest (root)
        
        // Also add intermediate paths that might not have files directly but have subdirectories
        for path in files_by_dir.keys() {
            for i in 0..path.len() {
                let sub = path[0..i].to_vec();
                if !files_by_dir.contains_key(&sub) {
                     // We add them to the list to process, but we handle dedup via BTreeMap logic implicitly 
                     // or we can just iterate carefully.
                     // Actually, a better way is to collect all unique directory paths.
                }
            }
        }
        
        // Let's collect all distinct directory paths that need to exist
        let mut all_dirs: BTreeMap<Vec<String>, Directory> = BTreeMap::new();
        
        // Populate leaf directories with files
        for (path, nodes) in files_by_dir {
            let dir = all_dirs.entry(path).or_insert_with(Directory::new);
            dir.files = nodes;
            // Sort files for canonical representation
            dir.files.sort_by(|a, b| a.name.cmp(&b.name));
        }

        // Now process bottom up to link directories
        // Get all paths, sort by length descending
        let mut paths: Vec<Vec<String>> = all_dirs.keys().cloned().collect();
        paths.sort_by(|a, b| b.len().cmp(&a.len()));

        let mut root_digest = Digest::new("".to_string(), 0);

        for path in paths {
            let dir = all_dirs.get(&path).cloned().unwrap();
            let digest = dir.digest();
            directories.insert(digest.clone(), dir.clone());

            if path.is_empty() {
                root_digest = digest;
            } else {
                let mut parent_path = path.clone();
                let dir_name = parent_path.pop().unwrap();
                
                let parent_dir = all_dirs.entry(parent_path).or_insert_with(Directory::new);
                parent_dir.directories.push(DirectoryNode {
                    name: dir_name,
                    digest,
                });
                // Keep directories sorted
                parent_dir.directories.sort_by(|a, b| a.name.cmp(&b.name));
            }
        }

        // If inputs was empty, we need an empty root
        if inputs.files.is_empty() {
             let empty_dir = Directory::new();
             let digest = empty_dir.digest();
             directories.insert(digest.clone(), empty_dir);
             root_digest = digest;
        }

        Ok(MerkleTree {
            root_digest,
            directories,
        })
    }
}


// Tests
#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::tasks::io::{ResolvedInputFile, ResolvedInputs};
    use std::path::PathBuf;

    #[test]
    fn test_empty_directory() {
        let dir = Directory::new();
        assert!(dir.files.is_empty());
        assert!(dir.directories.is_empty());
    }

    #[test]
    fn test_build_merkle_tree_flat() {
        let files = vec![
            ResolvedInputFile {
                rel_path: PathBuf::from("a.txt"),
                source_path: PathBuf::from("/tmp/a.txt"),
                sha256: "hash_a".to_string(),
                size: 100,
            },
            ResolvedInputFile {
                rel_path: PathBuf::from("b.txt"),
                source_path: PathBuf::from("/tmp/b.txt"),
                sha256: "hash_b".to_string(),
                size: 200,
            },
        ];
        let inputs = ResolvedInputs { files };
        let tree = MerkleTree::from_inputs(&inputs).unwrap();

        assert_eq!(tree.directories.len(), 1); // Only root
        let root = tree.directories.get(&tree.root_digest).unwrap();
        assert_eq!(root.files.len(), 2);
    }

    #[test]
    fn test_build_merkle_tree_nested() {
        let files = vec![
            ResolvedInputFile {
                rel_path: PathBuf::from("src/main.rs"),
                source_path: PathBuf::from("/tmp/src/main.rs"),
                sha256: "hash_main".to_string(),
                size: 10,
            },
            ResolvedInputFile {
                rel_path: PathBuf::from("README.md"),
                source_path: PathBuf::from("/tmp/README.md"),
                sha256: "hash_readme".to_string(),
                size: 20,
            },
        ];
        let inputs = ResolvedInputs { files };
        let tree = MerkleTree::from_inputs(&inputs).unwrap();

        // Root + src
        assert_eq!(tree.directories.len(), 2);
    }
}

