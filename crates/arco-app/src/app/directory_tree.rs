use super::{App, Event, Navigation};
use crate::{TreeData, TreeRow, display_path};
use arco_core::files::{self, Kind, SortKey};
use arco_core::ignore::IgnoreList;
use slint::{ModelRc, VecModel};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
enum Target {
    Desktop,
    Shell(String),
    Disk(PathBuf),
}

impl Target {
    fn address(&self) -> String {
        match self {
            Self::Desktop => "Desktop".into(),
            Self::Shell(name) => name.clone(),
            Self::Disk(path) => display_path(path),
        }
    }
}

struct Folder {
    target: Target,
    name: String,
    computer: bool,
}

impl Folder {
    fn disk(path: PathBuf, show_path: bool) -> Self {
        let name = match path.file_name() {
            Some(name) if !show_path => name.to_string_lossy().into_owned(),
            _ => display_path(&path),
        };
        Self {
            target: Target::Disk(path),
            name,
            computer: false,
        }
    }
}

struct Node {
    folder: Folder,
    parent: Option<usize>,
    children: Option<Vec<usize>>,
    expanded: bool,
}

pub(super) struct Tree {
    id: u64,
    directory: Option<PathBuf>,
    follow: bool,
    selected: usize,
    nodes: Vec<Node>,
    reopen: BTreeSet<Target>,
    visible: Vec<(usize, usize)>,
    model: Rc<VecModel<TreeRow>>,
    focused: bool,
    clicks: super::pointer::Clicks<usize>,
}

pub(super) struct Loaded {
    side: usize,
    id: u64,
    node: usize,
    children: io::Result<Vec<Folder>>,
}

impl Tree {
    fn new(id: u64, root: PathBuf, shell: bool) -> Self {
        let folder = if shell {
            Folder {
                target: Target::Desktop,
                name: "Desktop".into(),
                computer: false,
            }
        } else {
            Folder::disk(root.clone(), true)
        };
        let mut tree = Self {
            id,
            directory: Some(root),
            follow: shell,
            selected: 0,
            nodes: vec![Node {
                folder,
                parent: None,
                children: None,
                expanded: true,
            }],
            reopen: BTreeSet::new(),
            visible: Vec::new(),
            model: Rc::new(VecModel::default()),
            focused: !shell,
            clicks: super::pointer::Clicks::default(),
        };
        tree.rebuild();
        tree
    }

    fn is_shell(&self) -> bool {
        self.nodes[0].folder.target == Target::Desktop
    }

    fn rebuild(&mut self) {
        self.visible.clear();
        let mut stack = vec![(0, 0)];
        while let Some((index, depth)) = stack.pop() {
            let node = &self.nodes[index];
            if node.expanded
                && let Some(children) = &node.children
            {
                stack.extend(children.iter().rev().map(|&child| (child, depth + 1)));
            }
            self.visible.push((index, depth));
        }
        self.model.set_vec(
            self.visible
                .iter()
                .map(|&(index, depth)| {
                    let node = &self.nodes[index];
                    TreeRow {
                        title: node.folder.name.as_str().into(),
                        path: node.folder.target.address().into(),
                        depth: depth as i32,
                        expanded: node.expanded,
                        expandable: node
                            .children
                            .as_ref()
                            .is_none_or(|children| !children.is_empty()),
                    }
                })
                .collect::<Vec<_>>(),
        );
    }

    fn load_children(&mut self, index: usize, folders: Vec<Folder>) {
        let children = folders
            .into_iter()
            .map(|folder| {
                let id = self.nodes.len();
                let expanded = self.reopen.contains(&folder.target);
                self.nodes.push(Node {
                    folder,
                    parent: Some(index),
                    children: None,
                    expanded,
                });
                id
            })
            .collect();
        self.nodes[index].children = Some(children);
        self.follow_path();
    }

    fn selected_row(&self) -> usize {
        self.visible
            .iter()
            .position(|&(index, _)| index == self.selected)
            .unwrap_or(0)
    }

    fn follow_folder(&mut self, path: &Path) {
        self.directory = Some(path.to_owned());
        self.follow = true;
        self.nodes[0].expanded = true;
        self.follow_path();
    }

    fn follow_path(&mut self) {
        if self.follow
            && let Some(path) = self.directory.clone()
        {
            loop {
                self.rebuild();
                let normalized = normalize_disk_path(&path);
                let deepest = self
                    .visible
                    .iter()
                    .filter_map(|&(index, _)| {
                        let Target::Disk(folder) = &self.nodes[index].folder.target else {
                            return None;
                        };
                        let folder = normalize_disk_path(folder);
                        normalized.starts_with(&folder).then_some((
                            folder.components().count(),
                            index,
                            folder == normalized,
                        ))
                    })
                    .max_by_key(|&(depth, _, _)| depth);
                if let Some((_, index, exact)) = deepest {
                    self.selected = index;
                    if exact {
                        self.follow = false;
                        break;
                    }
                    let node = &mut self.nodes[index];
                    if node.expanded {
                        self.follow = node.children.is_none();
                        break;
                    }
                    node.expanded = true;
                    continue;
                }
                if !self.is_shell() || self.nodes[0].children.is_none() {
                    break;
                }
                let display = PathBuf::from(display_path(&path));
                if is_unc(&path)
                    && let Some(root) = display.ancestors().last()
                    && !self.nodes.iter().any(
                        |node| matches!(&node.folder.target, Target::Disk(path) if path == root),
                    )
                {
                    let index = self.nodes.len();
                    self.nodes.push(Node {
                        folder: Folder::disk(root.to_owned(), true),
                        parent: Some(0),
                        children: None,
                        expanded: true,
                    });
                    self.nodes[0].children.as_mut().unwrap().push(index);
                    continue;
                }
                if let Some((index, node)) = self
                    .nodes
                    .iter_mut()
                    .enumerate()
                    .find(|(_, node)| node.folder.computer)
                {
                    self.selected = index;
                    if node.expanded {
                        self.follow = node.children.is_none();
                        break;
                    }
                    node.expanded = true;
                    continue;
                }
                self.follow = false;
                break;
            }
        }
        self.rebuild();
    }

    fn invalidate(&mut self, id: u64) {
        self.id = id;
        self.reopen = self
            .nodes
            .iter()
            .filter(|node| node.expanded)
            .map(|node| node.folder.target.clone())
            .collect();
        self.nodes.truncate(1);
        self.nodes[0].children = None;
        self.selected = 0;
        self.nodes[0].expanded = true;
        self.follow = true;
        self.rebuild();
    }
}

fn normalize_disk_path(path: &Path) -> PathBuf {
    let name = display_path(path);
    PathBuf::from(if cfg!(windows) {
        name.to_lowercase()
    } else {
        name
    })
}

fn is_unc(path: &Path) -> bool {
    display_path(path).starts_with(r"\\")
}

impl App {
    pub(crate) fn toggle_tree(&mut self) {
        let side = self.active;
        if !self.current().is_local() {
            self.status("Öffne für den Ordnerbaum einen lokalen Ordner.");
            return;
        }
        if self.profile.separate_tree_mode > 0 {
            self.set_separate_trees(0);
            return;
        }
        if self.trees[side].take().is_none() {
            self.next_request += 1;
            self.trees[side] = Some(Tree::new(
                self.next_request,
                self.current().path.clone(),
                false,
            ));
        }
        self.publish_tree(side);
    }

    pub(crate) fn set_separate_trees(&mut self, mode: u8) {
        let mode = mode.min(2);
        self.profile.separate_tree_mode = mode;
        self.ui().set_separate_tree_mode(i32::from(mode));
        if mode == 0 {
            self.trees = [None, None];
        } else {
            self.update_trees();
        }
        for side in 0..2 {
            self.publish_tree(side);
        }
    }

    pub(super) fn publish_tree(&self, side: usize) {
        let data = self.trees[side]
            .as_ref()
            .filter(|tree| tree.is_shell() || self.panels[side].current().is_local())
            .map_or_else(TreeData::default, |tree| TreeData {
                rows: ModelRc::from(tree.model.clone()),
                cursor: tree.selected_row() as i32,
                shown: true,
                focused: tree.focused,
            });
        if side == 0 {
            self.ui().set_left_tree(data);
        } else {
            self.ui().set_right_tree(data);
        }
    }

    pub(super) fn leave_tree_focus(&mut self, side: usize) {
        if let Some(tree) = &mut self.trees[side]
            && tree.focused
        {
            tree.focused = false;
            self.publish_tree(side);
        }
    }

    pub(super) fn select_tree_row(&mut self, side: usize, row: usize, operation: i32) {
        if self.panels[side].loading {
            return;
        }
        let Some(tree) = &mut self.trees[side] else {
            return;
        };
        let Some(&(index, _)) = tree.visible.get(row) else {
            return;
        };
        let operation = if operation == 0 {
            if arco_windows::cursor_screen().is_some_and(|point| {
                tree.clicks.click(
                    index,
                    point,
                    std::time::Instant::now(),
                    arco_windows::is_double_click,
                )
            }) {
                2
            } else {
                0
            }
        } else {
            tree.clicks.clear();
            operation
        };
        tree.selected = index;
        tree.focused = true;
        tree.follow = false;
        self.set_active(side);
        match operation {
            1 => self.tree_key("tree-toggle-node"),
            2 => self.tree_key("open"),
            _ => false,
        };
        self.publish_tree(side);
    }

    pub(crate) fn tree_key(&mut self, command: &str) -> bool {
        let side = self.active;
        let local = self.current().is_local();
        let Some(tree) = &mut self.trees[side] else {
            return false;
        };
        if !tree.focused || (!local && !tree.is_shell()) {
            return false;
        }
        let node = &mut tree.nodes[tree.selected];
        match command {
            "open" => {
                match node.folder.target.clone() {
                    Target::Disk(path) => self.load(side, path, Navigation::Jump),
                    Target::Desktop => self.open_shell_panel(side, arco_windows::SHELL_DESKTOP),
                    Target::Shell(name) => self.open_shell_panel(side, &name),
                }
                return true;
            }
            "tree-toggle-node" => node.expanded = !node.expanded,
            "tree-expand" => {
                if node.expanded
                    && let Some(child) =
                        node.children.as_ref().and_then(|children| children.first())
                {
                    tree.selected = *child;
                }
                node.expanded = true;
            }
            "tree-collapse" => {
                if !node.expanded
                    && let Some(parent) = node.parent
                {
                    tree.selected = parent;
                }
                node.expanded = false;
            }
            "cursor-up" | "cursor-down" | "page-up" | "page-down" | "cursor-first"
            | "cursor-last" => {
                let last = tree.visible.len().saturating_sub(1);
                let row = tree.selected_row();
                let next = match command {
                    "cursor-up" => row.saturating_sub(1),
                    "cursor-down" => (row + 1).min(last),
                    "page-up" => row.saturating_sub(8),
                    "page-down" => (row + 8).min(last),
                    "cursor-first" => 0,
                    _ => last,
                };
                tree.selected = tree.visible[next].0;
            }
            _ => return false,
        }
        tree.follow = false;
        tree.rebuild();
        self.publish_tree(side);
        true
    }

    pub(crate) fn swap_trees(&mut self) {
        self.trees.swap(0, 1);
        // A worker still owns its original side. Reissue any outstanding branch there.
        for side in 0..2 {
            if self.tree_loading[side] {
                self.invalidate_tree(side);
            }
        }
    }

    pub(super) fn invalidate_tree(&mut self, side: usize) {
        if let Some(tree) = &mut self.trees[side] {
            self.next_request += 1;
            tree.invalidate(self.next_request);
            self.publish_tree(side);
        }
    }

    pub(super) fn update_trees(&mut self) {
        for side in 0..2 {
            let panel = &self.panels[side];
            if panel.loading {
                continue;
            }
            let shell = cfg!(windows) && self.profile.separate_tree_mode != 0;
            let local = panel.current().is_local();
            if !local && !shell {
                continue;
            }
            let path = panel.current().path.clone();
            let reset = self.trees[side].as_ref().map_or(self.profile.separate_tree_mode != 0, |tree| {
                tree.is_shell() != shell
                    || matches!(&tree.nodes[0].folder.target, Target::Disk(root) if !path.starts_with(root))
            });
            if reset {
                self.next_request += 1;
                self.trees[side] = Some(Tree::new(self.next_request, path.clone(), shell));
                self.publish_tree(side);
            }
            if local
                && let Some(tree) = &mut self.trees[side]
                && tree.directory.as_deref() != Some(&path)
            {
                tree.follow_folder(&path);
                self.publish_tree(side);
            }
            let Some(tree) = &self.trees[side] else {
                continue;
            };
            if self.tree_loading[side] {
                continue;
            }
            let Some(node) =
                tree.visible.iter().map(|&(index, _)| index).find(|&index| {
                    tree.nodes[index].expanded && tree.nodes[index].children.is_none()
                })
            else {
                continue;
            };
            let target = tree.nodes[node].folder.target.clone();
            let id = tree.id;
            let sender = self.sender.clone();
            let show_hidden = self.profile.show_hidden;
            let ignore = self.ignore_list();
            self.tree_loading[side] = true;
            std::thread::spawn(move || {
                let children = tree_children(&target, show_hidden, ignore.as_deref());
                let _ = sender.send(Event::TreeLoaded(Loaded {
                    side,
                    id,
                    node,
                    children,
                }));
            });
        }
    }

    pub(super) fn tree_loaded(&mut self, loaded: Loaded) {
        self.tree_loading[loaded.side] = false;
        let Some(tree) = self.trees[loaded.side]
            .as_mut()
            .filter(|tree| tree.id == loaded.id)
        else {
            return;
        };
        match loaded.children {
            Ok(children) => tree.load_children(loaded.node, children),
            Err(error) => {
                tree.load_children(loaded.node, Vec::new());
                self.status(format!("Ordnerbaum · {error}"));
            }
        }
        self.publish_tree(loaded.side);
    }
}

fn tree_children(
    target: &Target,
    show_hidden: bool,
    ignore: Option<&IgnoreList>,
) -> io::Result<Vec<Folder>> {
    if let Target::Disk(path) = target {
        if !files::regular_metadata(path)?.is_dir() {
            return Err(io::ErrorKind::NotADirectory.into());
        }
        let mut entries = files::list(path)?.entries;
        entries.retain(|entry| {
            entry.kind == Kind::Directory
                && (show_hidden || !entry.hidden)
                && !ignore.is_some_and(|rules| rules.local(&entry.path, true))
        });
        files::sort(&mut entries, SortKey::Name, false);
        return Ok(entries
            .into_iter()
            .map(|entry| Folder::disk(entry.path, false))
            .collect());
    }
    let parent = match target {
        Target::Shell(name) => Some(name.as_str()),
        _ => None,
    };
    Ok(arco_windows::shell_folders(parent, show_hidden)?
        .into_iter()
        .filter(|entry| {
            !entry
                .path
                .as_ref()
                .is_some_and(|path| ignore.is_some_and(|rules| rules.local(path, true)))
        })
        .map(|entry| Folder {
            target: entry
                .path
                .map_or(Target::Shell(entry.parsing_name), Target::Disk),
            name: entry.name,
            computer: entry.computer,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_tree_only_displays_expanded_and_unignored_subdirectories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let child = root.join("Ordner");
        let nested = child.join("Unterordner");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(root.join("skip")).unwrap();
        std::fs::write(root.join("datei.txt"), b"").unwrap();
        let rules = IgnoreList::parse("skip/").unwrap();
        let mut tree = Tree::new(1, root.to_path_buf(), false);
        let children = tree_children(&Target::Disk(root.into()), true, Some(&rules)).unwrap();
        tree.load_children(0, children);
        tree.load_children(1, tree_children(&Target::Disk(child), true, None).unwrap());
        assert_eq!(tree.visible, [(0, 0), (1, 1)]);
        tree.follow_folder(&nested);
        assert_eq!(tree.visible[2], (2, 2));
        assert_eq!(tree.selected, 2);
    }
}
