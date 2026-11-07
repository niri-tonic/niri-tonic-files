use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub columns: Vec<PathBuf>,
    pub active: usize,
}

#[derive(Debug)]
pub struct Navigation {
    current: Snapshot,
    back: Vec<Snapshot>,
    forward: Vec<Snapshot>,
}

impl Navigation {
    pub fn new(path: PathBuf) -> Self {
        Self {
            current: Snapshot {
                columns: vec![path],
                active: 0,
            },
            back: vec![],
            forward: vec![],
        }
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.current
    }
    pub fn active_path(&self) -> Option<&Path> {
        self.current
            .columns
            .get(self.current.active)
            .map(PathBuf::as_path)
    }
    pub fn activate(&mut self, index: usize) {
        if index < self.current.columns.len() {
            self.current.active = index;
        }
    }
    fn record(&mut self) {
        self.back.push(self.current.clone());
        if self.back.len() > 256 {
            self.back.remove(0);
        }
        self.forward.clear();
    }
    pub fn open(&mut self, path: PathBuf) {
        self.record();
        match self.current.columns.len() {
            0 => {
                self.current.columns.push(path);
                self.current.active = 0;
            }
            1 => {
                self.current.columns.push(path);
                self.current.active = 1;
            }
            _ => {
                let target = 1 - self.current.active;
                self.current.columns[target] = path;
                self.current.active = target;
            }
        }
    }
    pub fn close(&mut self, index: usize) -> bool {
        if index >= self.current.columns.len() {
            return false;
        }
        self.record();
        self.current.columns.remove(index);
        self.current.active = 0;
        true
    }
    pub fn back(&mut self) -> bool {
        if let Some(previous) = self.back.pop() {
            self.forward
                .push(std::mem::replace(&mut self.current, previous));
            true
        } else {
            false
        }
    }
    pub fn forward(&mut self) -> bool {
        if let Some(next) = self.forward.pop() {
            self.back.push(std::mem::replace(&mut self.current, next));
            true
        } else {
            false
        }
    }
    pub fn parent(&mut self) -> bool {
        let Some(parent) = self.active_path().and_then(crate::locations::parent) else {
            return false;
        };
        self.open(parent);
        true
    }
    pub fn restore(&mut self, columns: Vec<PathBuf>, active: usize) {
        if !columns.is_empty() && columns.len() <= 2 {
            self.current.active = active.min(columns.len() - 1);
            self.current.columns = columns;
        }
    }
    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }
    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }
}
