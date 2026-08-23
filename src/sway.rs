//! sway stays the source of truth for the window list and for focusing, exactly
//! as the shell script this replaces did (`swaymsg -t get_tree` + `[con_id=N]
//! focus`). The Wayland side only supplies pixels; the join between the two is
//! `foreign_toplevel_identifier`, which sway reports per view.

use swayipc::{Connection, Node, NodeType};

#[derive(Clone, Debug)]
pub struct Win {
    pub con_id: i64,
    pub app: String,
    pub title: String,
    /// ext-foreign-toplevel-list-v1 identifier; the key we match capture
    /// sources on.
    pub ft_id: String,
}

impl Win {
    /// "title · app", the label rofigrid was given (app dropped when empty).
    /// Used once tiles are labelled.
    #[allow(dead_code)]
    pub fn label(&self) -> String {
        if self.app.is_empty() {
            self.title.clone()
        } else {
            format!("{} · {}", self.title, self.app)
        }
    }
}

/// Every view in the tree, in tree order (same traversal the jq filter did, so
/// the grid keeps the ordering the muscle memory expects).
pub fn windows(conn: &mut Connection) -> Result<Vec<Win>, swayipc::Error> {
    let mut out = Vec::new();
    collect(&conn.get_tree()?, &mut out);
    Ok(out)
}

fn collect(node: &Node, out: &mut Vec<Win>) {
    let is_con = matches!(node.node_type, NodeType::Con | NodeType::FloatingCon);
    let class = node
        .window_properties
        .as_ref()
        .and_then(|p| p.class.clone());
    if is_con && (node.app_id.is_some() || class.is_some()) {
        // A view with no identifier can't be captured, but it still belongs in
        // the list: it gets a tile with no thumbnail.
        out.push(Win {
            con_id: node.id,
            app: node.app_id.clone().or(class).unwrap_or_default(),
            title: node.name.clone().unwrap_or_default(),
            ft_id: node.foreign_toplevel_identifier.clone().unwrap_or_default(),
        });
    }
    for child in node.nodes.iter().chain(node.floating_nodes.iter()) {
        collect(child, out);
    }
}

pub fn focus(conn: &mut Connection, con_id: i64) -> Result<(), swayipc::Error> {
    for res in conn.run_command(format!("[con_id={con_id}] focus"))? {
        res?;
    }
    Ok(())
}
