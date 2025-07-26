#[derive(Debug, Clone)]
pub struct TreeNode {
    pub name: String,
    pub size: u64,
    pub is_directory: bool,
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    pub fn new_file(name: String, size: u64) -> Self {
        Self {
            name,
            size,
            is_directory: false,
            children: Vec::new(),
        }
    }
    
    pub fn new_directory(name: String) -> Self {
        Self {
            name,
            size: 0,
            is_directory: true,
            children: Vec::new(),
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
}