// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeMap;

use accessibility_sys::pid_t;
use itertools::Itertools;
use serde::{Deserialize, Serialize};

use super::tree::{NodeId, NodeMap};
use crate::actor::app::WindowId;
use crate::collections::BTreeExt;
use crate::model::layout_tree::TreeEvent;

/// Maintains a two-way mapping between leaf nodes and window ids.
///
/// A window can be present in more than one layout at once (for example the
/// same window on several screen sizes). Which layout a node belongs to is
/// determined by which root it descends from, so it is not stored here;
/// callers that want the node in a particular layout filter by tree position.
#[derive(Default, Serialize, Deserialize)]
pub struct Window {
    windows: slotmap::SecondaryMap<NodeId, WindowId>,
    window_nodes: BTreeMap<WindowId, Vec<NodeId>>,
}

impl Window {
    pub fn at(&self, node: NodeId) -> Option<WindowId> {
        self.windows.get(node).copied()
    }

    /// Returns every node mapped to `wid`, across all layouts.
    pub(super) fn nodes_for(&self, wid: WindowId) -> impl Iterator<Item = NodeId> + use<'_> {
        self.window_nodes.get(&wid).into_iter().flatten().copied()
    }

    pub fn set_window(&mut self, node: NodeId, wid: WindowId) {
        let existing = self.windows.insert(node, wid);
        assert!(
            existing.is_none(),
            "Attempted to overwrite window for node {node:?} from {existing:?} to {wid:?}"
        );
        self.window_nodes.entry(wid).or_default().push(node);
    }

    pub fn swap_windows(&mut self, node_a: NodeId, node_b: NodeId) {
        let wid_a = self.windows.get(node_a).copied();
        let wid_b = self.windows.get(node_b).copied();
        match (wid_a, wid_b) {
            (Some(a), Some(b)) => {
                self.windows[node_a] = b;
                self.windows[node_b] = a;
                for node in self.window_nodes.get_mut(&a).into_iter().flatten() {
                    if *node == node_a {
                        *node = node_b;
                    }
                }
                for node in self.window_nodes.get_mut(&b).into_iter().flatten() {
                    if *node == node_b {
                        *node = node_a;
                    }
                }
            }
            _ => {}
        }
    }

    pub fn set_capacity(&mut self, capacity: usize) {
        self.windows.set_capacity(capacity);
        // There's not currently a stable way to do this for BTreeMap.
    }

    pub(super) fn window_ids(&self) -> impl Iterator<Item = WindowId> {
        self.window_nodes.keys().copied()
    }

    pub(super) fn pids(&self) -> impl Iterator<Item = pid_t> {
        self.window_ids().map(|wid| wid.pid).dedup()
    }

    pub(super) fn take_nodes_for(&mut self, wid: WindowId) -> impl Iterator<Item = NodeId> + use<> {
        self.window_nodes.remove(&wid).unwrap_or_default().into_iter()
    }

    pub(super) fn take_nodes_for_app(
        &mut self,
        pid: pid_t,
    ) -> impl Iterator<Item = (WindowId, NodeId)> + use<> {
        let removed = self.window_nodes.remove_all_for_pid(pid);
        removed
            .into_iter()
            .flat_map(|(wid, nodes)| nodes.into_iter().map(move |node| (wid, node)))
    }

    pub(super) fn handle_event(&mut self, map: &NodeMap, event: TreeEvent) {
        use TreeEvent::*;
        match event {
            AddedToForest(_) => (),
            AddedToParent(node) => debug_assert!(
                self.windows.get(node.parent(map).unwrap()).is_none(),
                "Window nodes are not allowed to have children: {:?}/{:?}",
                node.parent(map).unwrap(),
                node
            ),
            Copied { src, dest } => {
                if let Some(&wid) = self.windows.get(src) {
                    self.set_window(dest, wid);
                }
            }
            RemovingFromParent(_) => (),
            RemovedFromForest(node) => {
                if let Some(wid) = self.windows.remove(node) {
                    if let Some(nodes) = self.window_nodes.get_mut(&wid) {
                        nodes.retain(|&n| n != node);
                        if nodes.is_empty() {
                            self.window_nodes.remove(&wid);
                        }
                    }
                }
            }
        }
    }
}
