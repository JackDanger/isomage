#[derive(Debug, Clone)]
pub struct TreeNode {
    pub name: String,
    pub size: u64,
    pub is_directory: bool,
    pub children: Vec<TreeNode>,
    pub file_location: Option<u64>,
    pub file_length: Option<u64>,
}

impl TreeNode {
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
    
    pub fn add_child(&mut self, child: TreeNode) {
        self.children.push(child);
    }
    
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
    
    pub fn find_node(&self, path: &str) -> Option<&TreeNode> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return Some(self);
        }
        
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() == 1 && parts[0] == self.name {
            return Some(self);
        }
        
        if parts.len() > 0 {
            let first_part = parts[0];
            for child in &self.children {
                if child.name == first_part {
                    if parts.len() == 1 {
                        return Some(child);
                    } else {
                        let remaining_path = parts[1..].join("/");
                        return child.find_node(&remaining_path);
                    }
                }
            }
        }
        
        None
    }
}