//! Deterministic, toolkit-independent projection of a workspace tree.

use super::{Axis, Node, PaneId};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl LayoutRect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite())
            && self.width >= 0.0
            && self.height >= 0.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneLayout {
    pub pane_id: PaneId,
    pub bounds: LayoutRect,
    pub focused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinSizePolicy {
    pub width: f32,
    pub height: f32,
}

impl MinSizePolicy {
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

impl Default for MinSizePolicy {
    fn default() -> Self {
        Self::new(1.0, 1.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProjectionError {
    InvalidBounds,
    InvalidMinimumSize,
    InvalidWeights {
        expected: usize,
        actual: usize,
    },
    InvalidWeight {
        index: usize,
        value: f32,
    },
    InsufficientSpace {
        axis: Axis,
        required: f32,
        available: f32,
    },
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ProjectionError {}

/// Projects every pane in `node` into a stable depth-first list of rectangles.
/// Split weights are normalized for layout, so callers may provide any finite,
/// positive weights rather than pre-normalizing them in the workspace model.
pub fn project_node<C>(
    node: &Node<C>,
    bounds: LayoutRect,
    focused: Option<PaneId>,
    minimum: MinSizePolicy,
) -> Result<Vec<PaneLayout>, ProjectionError> {
    if !bounds.valid() {
        return Err(ProjectionError::InvalidBounds);
    }
    if !minimum.width.is_finite()
        || !minimum.height.is_finite()
        || minimum.width < 0.0
        || minimum.height < 0.0
    {
        return Err(ProjectionError::InvalidMinimumSize);
    }

    let mut output = Vec::new();
    project(node, bounds, focused, minimum, &mut output)?;
    Ok(output)
}

fn project<C>(
    node: &Node<C>,
    bounds: LayoutRect,
    focused: Option<PaneId>,
    minimum: MinSizePolicy,
    output: &mut Vec<PaneLayout>,
) -> Result<(), ProjectionError> {
    match node {
        Node::Pane(pane) => output.push(PaneLayout {
            pane_id: pane.id,
            bounds,
            focused: focused == Some(pane.id),
        }),
        Node::Split {
            axis,
            weights,
            children,
            ..
        } => {
            if weights.len() != children.len() {
                return Err(ProjectionError::InvalidWeights {
                    expected: children.len(),
                    actual: weights.len(),
                });
            }
            if children.is_empty() {
                return Ok(());
            }
            for (index, weight) in weights.iter().copied().enumerate() {
                if !weight.is_finite() || weight <= 0.0 {
                    return Err(ProjectionError::InvalidWeight {
                        index,
                        value: weight,
                    });
                }
            }

            let available = match axis {
                Axis::Horizontal => bounds.width,
                Axis::Vertical => bounds.height,
            };
            let minimum_extent = match axis {
                Axis::Horizontal => minimum.width,
                Axis::Vertical => minimum.height,
            };
            let required = minimum_extent * children.len() as f32;
            if required > available + f32::EPSILON {
                return Err(ProjectionError::InsufficientSpace {
                    axis: *axis,
                    required,
                    available,
                });
            }

            let extents = distribute(available, weights, minimum_extent);
            let mut cursor = match axis {
                Axis::Horizontal => bounds.x,
                Axis::Vertical => bounds.y,
            };
            for (index, child) in children.iter().enumerate() {
                let end = if index + 1 == children.len() {
                    match axis {
                        Axis::Horizontal => bounds.x + available,
                        Axis::Vertical => bounds.y + available,
                    }
                } else {
                    cursor + extents[index]
                };
                let child_bounds = match axis {
                    Axis::Horizontal => {
                        LayoutRect::new(cursor, bounds.y, end - cursor, bounds.height)
                    }
                    Axis::Vertical => LayoutRect::new(bounds.x, cursor, bounds.width, end - cursor),
                };
                project(child, child_bounds, focused, minimum, output)?;
                cursor = end;
            }
        }
    }
    Ok(())
}

fn distribute(available: f32, weights: &[f32], minimum: f32) -> Vec<f32> {
    let sum: f32 = weights.iter().sum();
    let mut extents: Vec<f32> = weights
        .iter()
        .map(|weight| available * *weight / sum)
        .collect();
    let mut fixed = vec![false; weights.len()];
    let mut remaining = available;
    let mut remaining_weight = sum;

    loop {
        let mut changed = false;
        for index in 0..weights.len() {
            if !fixed[index] && extents[index] < minimum {
                extents[index] = minimum;
                fixed[index] = true;
                remaining -= minimum;
                remaining_weight -= weights[index];
                changed = true;
            }
        }
        if !changed {
            break;
        }
        for index in 0..weights.len() {
            if !fixed[index] {
                extents[index] = remaining * weights[index] / remaining_weight;
            }
        }
    }
    extents
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{Pane, SplitId};

    fn pane(id: u64) -> Node<&'static str> {
        Node::Pane(Pane {
            id: PaneId::new(id),
            content: "pane",
        })
    }
    fn split(
        axis: Axis,
        weights: Vec<f32>,
        children: Vec<Node<&'static str>>,
    ) -> Node<&'static str> {
        Node::Split {
            id: SplitId::new(1),
            axis,
            weights,
            children,
        }
    }
    fn full() -> LayoutRect {
        LayoutRect::new(0.0, 0.0, 100.0, 80.0)
    }

    #[test]
    fn horizontal_and_vertical_splits_cover_bounds() {
        let horizontal = split(Axis::Horizontal, vec![1.0, 3.0], vec![pane(1), pane(2)]);
        let result = project_node(&horizontal, full(), None, MinSizePolicy::new(0.0, 0.0)).unwrap();
        assert_eq!(result[0].bounds, LayoutRect::new(0.0, 0.0, 25.0, 80.0));
        assert_eq!(result[1].bounds, LayoutRect::new(25.0, 0.0, 75.0, 80.0));
        let vertical = split(Axis::Vertical, vec![1.0, 1.0], vec![pane(3), pane(4)]);
        let result = project_node(&vertical, full(), None, MinSizePolicy::new(0.0, 0.0)).unwrap();
        assert_eq!(result[0].bounds, LayoutRect::new(0.0, 0.0, 100.0, 40.0));
        assert_eq!(result[1].bounds, LayoutRect::new(0.0, 40.0, 100.0, 40.0));
    }

    #[test]
    fn nested_split_and_focus_are_deterministic() {
        let tree = split(
            Axis::Horizontal,
            vec![2.0, 1.0],
            vec![
                split(Axis::Vertical, vec![1.0, 3.0], vec![pane(1), pane(2)]),
                pane(3),
            ],
        );
        let a = project_node(
            &tree,
            full(),
            Some(PaneId::new(2)),
            MinSizePolicy::new(0.0, 0.0),
        )
        .unwrap();
        let b = project_node(
            &tree,
            full(),
            Some(PaneId::new(2)),
            MinSizePolicy::new(0.0, 0.0),
        )
        .unwrap();
        assert_eq!(a, b);
        assert!(a[1].focused);
        assert!((a[0].bounds.height + a[1].bounds.height - 80.0).abs() < 0.0001);
        assert!((a[2].bounds.x + a[2].bounds.width - 100.0).abs() < 0.0001);
    }

    #[test]
    fn minimum_sizes_are_clamped() {
        let tree = split(Axis::Horizontal, vec![99.0, 1.0], vec![pane(1), pane(2)]);
        let result = project_node(
            &tree,
            LayoutRect::new(0.0, 0.0, 100.0, 10.0),
            None,
            MinSizePolicy::new(20.0, 0.0),
        )
        .unwrap();
        assert_eq!(result[1].bounds.width, 20.0);
        assert_eq!(result[0].bounds.width, 80.0);
        assert!(matches!(
            project_node(
                &tree,
                LayoutRect::new(0.0, 0.0, 30.0, 10.0),
                None,
                MinSizePolicy::new(20.0, 0.0)
            ),
            Err(ProjectionError::InsufficientSpace { .. })
        ));
    }

    #[test]
    fn invalid_input_is_explicit() {
        assert_eq!(
            project_node(
                &pane(1),
                LayoutRect::new(0.0, 0.0, -1.0, 1.0),
                None,
                Default::default()
            ),
            Err(ProjectionError::InvalidBounds)
        );
        let tree = split(
            Axis::Horizontal,
            vec![1.0, f32::NAN],
            vec![pane(1), pane(2)],
        );
        assert!(matches!(
            project_node(&tree, full(), None, Default::default()),
            Err(ProjectionError::InvalidWeight { index: 1, .. })
        ));
    }
}
