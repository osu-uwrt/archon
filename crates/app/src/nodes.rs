//! The nodes on the graph: each board's node and the rest, and what each one publishes, subscribes to and serves.
//! For "is the board's node up, and is it listening on the topic I'm sending to?"

use archon_ros::{Graph, NodeInfo};
use egui::{RichText, ScrollArea, Sense, Ui, vec2};

#[derive(Default)]
pub struct NodesState {
    /// The node picked, by name, so it stays picked as others come and go
    pub selected: Option<String>,
}

impl NodesState {
    pub fn move_selection(&mut self, graph: &Graph, by: isize) {
        if graph.nodes.is_empty() {
            return;
        }
        // From the one shown as picked: the first, when none has been
        let from = self.index(graph).unwrap_or(0) as isize;
        let at = (from + by).clamp(0, graph.nodes.len() as isize - 1) as usize;
        self.selected = Some(graph.nodes[at].name.clone());
    }

    fn index(&self, graph: &Graph) -> Option<usize> {
        self.selected.as_ref().and_then(|name| graph.nodes.iter().position(|n| n.name == *name))
    }
}

pub enum Action {
    /// Echo a topic, with its type
    OpenTopic(String, String),
}

pub fn show(ui: &mut Ui, pane: usize, graph: &Graph, state: &mut NodesState) -> Option<Action> {
    let mut action = None;
    let visuals = ui.visuals().clone();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Nodes").strong());
        ui.label(RichText::new(format!("{} on the graph", graph.nodes.len())).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new("j/k pick · click a topic to echo it").small().weak());
        });
    });
    ui.separator();
    if graph.nodes.is_empty() {
        ui.add_space(12.0);
        ui.label(RichText::new("No nodes yet. A board's node appears once its agent has a session.").weak());
        return None;
    }
    let selected = state.index(graph).unwrap_or(0);
    ui.columns(2, |columns| {
        ScrollArea::vertical().id_salt(("nodes-list", pane)).auto_shrink(false).show(&mut columns[0], |ui| {
            for (i, node) in graph.nodes.iter().enumerate() {
                let height = ui.text_style_height(&egui::TextStyle::Body) * 2.2;
                let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
                if i == selected {
                    ui.painter().rect_filled(rect, 4.0, visuals.selection.bg_fill.gamma_multiply(0.6));
                }
                if response.clicked() {
                    state.selected = Some(node.name.clone());
                }
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 2.0))));
                child.label(RichText::new(&node.name).strong());
                child.label(RichText::new(counts(node)).small().weak());
            }
        });
        let node = &graph.nodes[selected];
        ScrollArea::vertical().id_salt(("nodes-detail", pane)).auto_shrink(false).show(&mut columns[1], |ui| {
            ui.label(RichText::new(&node.name).heading());
            for (title, list, topics) in [
                ("Publishes", &node.publishes, true),
                ("Subscribes to", &node.subscribes, true),
                ("Serves", &node.services, false),
            ] {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("{title} ({})", list.len())).small().weak());
                if list.is_empty() {
                    ui.label(RichText::new("nothing").weak());
                }
                for (name, ty) in list {
                    ui.horizontal(|ui| {
                        if topics {
                            let link = ui.link(RichText::new(name).monospace()).on_hover_text("Echo it");
                            if link.clicked() {
                                action = Some(Action::OpenTopic(name.clone(), ty.clone()));
                            }
                        } else {
                            ui.label(RichText::new(name).monospace());
                        }
                        ui.label(RichText::new(ty).small().weak());
                    });
                }
            }
        });
    });
    action
}

/// Like "4 pub · 2 sub · 1 srv", short enough for a narrow pane
fn counts(node: &NodeInfo) -> String {
    format!("{} pub · {} sub · {} srv", node.publishes.len(), node.subscribes.len(), node.services.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> Graph {
        let node = |name: &str| NodeInfo { name: name.to_owned(), ..Default::default() };
        Graph { topics: Vec::new(), nodes: vec![node("/a"), node("/b"), node("/c")] }
    }

    #[test]
    fn selection_follows_the_node_and_stops_at_the_ends() {
        let mut state = NodesState::default();
        let g = graph();
        state.move_selection(&g, 1);
        assert_eq!(state.selected.as_deref(), Some("/b"));
        state.move_selection(&g, 5);
        assert_eq!(state.selected.as_deref(), Some("/c"));
        // /a goes away: /c is still the one picked
        let mut fewer = g.clone();
        fewer.nodes.remove(0);
        assert_eq!(state.index(&fewer), Some(1));
        assert_eq!(
            counts(&NodeInfo { publishes: vec![("/x".into(), "t".into())], ..Default::default() }),
            "1 pub · 0 sub · 0 srv"
        );
    }
}
