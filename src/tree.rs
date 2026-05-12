//! The [`TreeNode`] model: a single in-memory representation of a
//! parsed disc's directory tree, shared by both parsers.
//!
//! Every parser produces a [`TreeNode`] tree rooted at `"/"`. Files
//! carry a `(file_location, file_length)` byte-range pointing into the
//! original image; the bytes themselves are not loaded until
//! [`crate::cat_node`] or [`crate::extract_node`] asks for them.

/// One entry in a parsed disc: either a directory (with `children`) or a
/// file (with `file_location` and `file_length` pointing into the image).
///
/// The root of the tree is always a directory named `"/"`. Sizes for
/// directories are populated by [`TreeNode::calculate_directory_size`]
/// after the tree is built — until then a directory's `size` is `0`.
#[derive(Debug, Clone)]
pub struct TreeNode {
    /// Last component of the entry's path. The root is named `"/"`.
    pub name: String,
    /// File length in bytes for files; total of all descendants for
    /// directories (after [`calculate_directory_size`](Self::calculate_directory_size)).
    pub size: u64,
    /// `true` for directories, `false` for regular files.
    pub is_directory: bool,
    /// Direct children. Empty for files.
    pub children: Vec<TreeNode>,
    /// Byte offset of the file's data inside the original image, if known.
    pub file_location: Option<u64>,
    /// File length in bytes, if known. Equal to `size` for files.
    pub file_length: Option<u64>,
}

impl TreeNode {
    /// Construct a file node without a location. Useful for tests or
    /// for parsers that resolve the location in a later pass.
    pub fn new_file(name: String, size: u64) -> Self {
        Self {
            name,
            size,
            is_directory: false,
            children: Vec::new(),
            file_location: None,
            file_length: None,
        }
    }

    /// Construct a file node with both its byte-range location and its
    /// length stamped in. This is the constructor parsers should
    /// generally use for real files.
    pub fn new_file_with_location(name: String, size: u64, location: u64, length: u64) -> Self {
        Self {
            name,
            size,
            is_directory: false,
            children: Vec::new(),
            file_location: Some(location),
            file_length: Some(length),
        }
    }

    /// Construct an empty directory node. `size` is `0` until
    /// [`calculate_directory_size`](Self::calculate_directory_size) is
    /// called.
    pub fn new_directory(name: String) -> Self {
        Self {
            name,
            size: 0,
            is_directory: true,
            children: Vec::new(),
            file_location: None,
            file_length: None,
        }
    }

    /// Append a child to this directory. Order is preserved.
    pub fn add_child(&mut self, child: TreeNode) {
        self.children.push(child);
    }

    /// Recursively fill in `size` for every directory in the subtree:
    /// each directory's `size` becomes the sum of its descendants'
    /// sizes. File nodes are left alone.
    ///
    /// Parsers call this once after the tree is fully built.
    pub fn calculate_directory_size(&mut self) {
        if self.is_directory {
            let mut total_size = 0;
            for child in &mut self.children {
                child.calculate_directory_size();
                total_size += child.size;
            }
            self.size = total_size;
        }
    }

    /// Look up a node by slash-separated path relative to this node.
    /// Leading slashes are tolerated, so `find_node("/etc/hostname")`
    /// and `find_node("etc/hostname")` are equivalent.
    ///
    /// Returns `None` if any path segment doesn't resolve. Empty paths
    /// and the literal `"/"` both return the receiver.
    ///
    /// # Example
    ///
    /// ```
    /// use isomage::TreeNode;
    /// let mut root = TreeNode::new_directory("/".to_string());
    /// let mut etc = TreeNode::new_directory("etc".to_string());
    /// etc.add_child(TreeNode::new_file("hostname".to_string(), 18));
    /// root.add_child(etc);
    ///
    /// assert!(root.find_node("etc/hostname").is_some());
    /// assert!(root.find_node("/etc/hostname").is_some());
    /// assert!(root.find_node("etc/missing").is_none());
    /// ```
    pub fn find_node(&self, path: &str) -> Option<&TreeNode> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return Some(self);
        }

        if path == self.name {
            return Some(self);
        }

        let (first, rest) = match path.find('/') {
            Some(pos) => (&path[..pos], Some(&path[pos + 1..])),
            None => (path, None),
        };

        for child in &self.children {
            if child.name == first {
                return match rest {
                    Some(remaining) => child.find_node(remaining),
                    None => Some(child),
                };
            }
        }

        None
    }
}
