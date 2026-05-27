pub(super) const DIVIDER_THICKNESS: f32 = 1.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TerminalId(u64);

impl TerminalId {
    pub(super) fn new(id: u64) -> Self {
        Self(id)
    }

    pub(super) fn as_u64(self) -> u64 {
        self.0
    }
}

pub(super) struct TerminalTab {
    pub(super) title: String,
    pub(super) layout: PaneNode,
    pub(super) active_pane: TerminalId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PaneNode {
    Leaf(TerminalId),
    Split {
        axis: SplitAxis,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
    },
}

impl PaneNode {
    pub(super) fn split_leaf(
        &mut self,
        target: TerminalId,
        axis: SplitAxis,
        new_leaf: TerminalId,
    ) -> bool {
        match self {
            PaneNode::Leaf(id) if *id == target => {
                *self = PaneNode::Split {
                    axis,
                    first: Box::new(PaneNode::Leaf(target)),
                    second: Box::new(PaneNode::Leaf(new_leaf)),
                };
                true
            }
            PaneNode::Leaf(_) => false,
            PaneNode::Split { first, second, .. } => {
                first.split_leaf(target, axis, new_leaf)
                    || second.split_leaf(target, axis, new_leaf)
            }
        }
    }

    pub(super) fn contains(&self, terminal_id: TerminalId) -> bool {
        match self {
            PaneNode::Leaf(id) => *id == terminal_id,
            PaneNode::Split { first, second, .. } => {
                first.contains(terminal_id) || second.contains(terminal_id)
            }
        }
    }

    pub(super) fn active_or_first_leaf(&self, active: TerminalId) -> Option<TerminalId> {
        if self.contains(active) {
            Some(active)
        } else {
            self.first_leaf()
        }
    }

    pub(super) fn first_leaf(&self) -> Option<TerminalId> {
        match self {
            PaneNode::Leaf(id) => Some(*id),
            PaneNode::Split { first, .. } => first.first_leaf(),
        }
    }

    pub(super) fn leaf_ids(&self, ids: &mut Vec<TerminalId>) {
        match self {
            PaneNode::Leaf(id) => ids.push(*id),
            PaneNode::Split { first, second, .. } => {
                first.leaf_ids(ids);
                second.leaf_ids(ids);
            }
        }
    }

    pub(super) fn leaf_count(&self) -> usize {
        match self {
            PaneNode::Leaf(_) => 1,
            PaneNode::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    pub(super) fn remove_leaf(self, target: TerminalId) -> Option<PaneNode> {
        match self {
            PaneNode::Leaf(id) if id == target => None,
            PaneNode::Leaf(_) => Some(self),
            PaneNode::Split {
                axis,
                first,
                second,
            } => match (first.remove_leaf(target), second.remove_leaf(target)) {
                (None, None) => None,
                (Some(node), None) | (None, Some(node)) => Some(node),
                (Some(f), Some(s)) => Some(PaneNode::Split {
                    axis,
                    first: Box::new(f),
                    second: Box::new(s),
                }),
            },
        }
    }
}

impl Default for PaneNode {
    fn default() -> Self {
        // TerminalId(0) is never allocated, so this is a placeholder for std::mem::take.
        PaneNode::Leaf(TerminalId::new(0))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SplitAxis {
    Row,
    Column,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PixelRegion {
    pub(super) width: f32,
    pub(super) height: f32,
}

pub(super) fn split_region(region: PixelRegion, axis: SplitAxis) -> (PixelRegion, PixelRegion) {
    match axis {
        SplitAxis::Row => {
            let width = ((region.width - DIVIDER_THICKNESS) / 2.0).max(1.0);
            (
                PixelRegion {
                    width,
                    height: region.height,
                },
                PixelRegion {
                    width,
                    height: region.height,
                },
            )
        }
        SplitAxis::Column => {
            let height = ((region.height - DIVIDER_THICKNESS) / 2.0).max(1.0);
            (
                PixelRegion {
                    width: region.width,
                    height,
                },
                PixelRegion {
                    width: region.width,
                    height,
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_leaf_replaces_target_with_ordered_split() {
        let first = TerminalId::new(1);
        let second = TerminalId::new(2);
        let mut layout = PaneNode::Leaf(first);

        assert!(layout.split_leaf(first, SplitAxis::Row, second));

        assert_eq!(
            layout,
            PaneNode::Split {
                axis: SplitAxis::Row,
                first: Box::new(PaneNode::Leaf(first)),
                second: Box::new(PaneNode::Leaf(second)),
            }
        );
    }

    #[test]
    fn remove_leaf_collapses_split_to_sibling() {
        let layout = PaneNode::Split {
            axis: SplitAxis::Row,
            first: Box::new(PaneNode::Leaf(TerminalId::new(1))),
            second: Box::new(PaneNode::Leaf(TerminalId::new(2))),
        };

        assert_eq!(
            layout.remove_leaf(TerminalId::new(1)),
            Some(PaneNode::Leaf(TerminalId::new(2)))
        );
    }

    #[test]
    fn remove_leaf_returns_none_when_tree_empties() {
        let layout = PaneNode::Leaf(TerminalId::new(1));
        assert_eq!(layout.remove_leaf(TerminalId::new(1)), None);
    }

    #[test]
    fn remove_leaf_collapses_nested_split() {
        let layout = PaneNode::Split {
            axis: SplitAxis::Row,
            first: Box::new(PaneNode::Leaf(TerminalId::new(1))),
            second: Box::new(PaneNode::Split {
                axis: SplitAxis::Column,
                first: Box::new(PaneNode::Leaf(TerminalId::new(2))),
                second: Box::new(PaneNode::Leaf(TerminalId::new(3))),
            }),
        };

        assert_eq!(
            layout.remove_leaf(TerminalId::new(2)),
            Some(PaneNode::Split {
                axis: SplitAxis::Row,
                first: Box::new(PaneNode::Leaf(TerminalId::new(1))),
                second: Box::new(PaneNode::Leaf(TerminalId::new(3))),
            })
        );
    }

    #[test]
    fn leaf_ids_preserve_focus_order() {
        let layout = PaneNode::Split {
            axis: SplitAxis::Row,
            first: Box::new(PaneNode::Leaf(TerminalId::new(1))),
            second: Box::new(PaneNode::Split {
                axis: SplitAxis::Column,
                first: Box::new(PaneNode::Leaf(TerminalId::new(2))),
                second: Box::new(PaneNode::Leaf(TerminalId::new(3))),
            }),
        };
        let mut ids = Vec::new();

        layout.leaf_ids(&mut ids);

        assert_eq!(
            ids,
            vec![TerminalId::new(1), TerminalId::new(2), TerminalId::new(3)]
        );
    }

    #[test]
    fn split_region_accounts_for_divider() {
        let (left, right) = split_region(
            PixelRegion {
                width: 101.0,
                height: 80.0,
            },
            SplitAxis::Row,
        );

        assert_eq!(
            (left, right),
            (
                PixelRegion {
                    width: 50.0,
                    height: 80.0,
                },
                PixelRegion {
                    width: 50.0,
                    height: 80.0,
                },
            )
        );
    }
}
