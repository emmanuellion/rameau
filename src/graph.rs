//! Lane layout for the commit graph: input is commits in topological order
//! (children before parents), output is, for each row, where its node sits and
//! which line segments cross that row.

/// A segment inside one row. `Top` goes from the top edge at lane `from` to
/// the row center at lane `to`; `Bottom` from the center at `from` to the bottom edge at `to`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Half {
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub half: Half,
    pub from: usize,
    pub to: usize,
    pub color: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Row {
    pub lane: usize,
    pub color: usize,
    pub segments: Vec<Segment>,
}

/// Number of lane colors in the UI palette.
pub const PALETTE: usize = 10;

/// `commits[i] = (id, parent ids)`. Ids are any comparable value (git Oids in the app).
/// `color_of(id)` gives the color of a lane starting at `id`: deriving it from the commit
/// (or its branch name) keeps colors stable when new commits arrive.
pub fn layout<T: PartialEq + Clone>(commits: &[(T, Vec<T>)], color_of: impl Fn(&T) -> usize) -> Vec<Row> {
    // Each lane waits for a given commit and carries a color.
    let mut lanes: Vec<Option<(T, usize)>> = Vec::new();
    let free_slot = |lanes: &mut Vec<Option<(T, usize)>>| {
        lanes.iter().position(Option::is_none).unwrap_or_else(|| {
            lanes.push(None);
            lanes.len() - 1
        })
    };

    // A new lane avoids the colors of lanes already on screen (when there are few enough).
    let distinct = |lanes: &[Option<(T, usize)>], c: usize| {
        (0..PALETTE)
            .map(|k| (c + k) % PALETTE)
            .find(|c| !lanes.iter().flatten().any(|(_, used)| used % PALETTE == *c))
            .unwrap_or(c % PALETTE)
    };
    let mut rows = Vec::with_capacity(commits.len());
    for (id, parents) in commits {
        let waiting = |l: &Option<(T, usize)>| l.as_ref().is_some_and(|(w, _)| w == id);
        let lane = match lanes.iter().position(waiting) {
            Some(i) => i,
            None => free_slot(&mut lanes),
        };
        let color = lanes[lane].as_ref().map_or_else(|| distinct(&lanes, color_of(id)), |(_, c)| *c);
        let mut segments = Vec::new();

        // Lines coming from above: lanes waiting for this commit converge on it, others pass through.
        for (i, l) in lanes.iter_mut().enumerate() {
            let Some((w, c)) = l.as_ref() else { continue };
            let c = *c;
            if w == id {
                segments.push(Segment { half: Half::Top, from: i, to: lane, color: c });
                *l = None;
            } else {
                segments.push(Segment { half: Half::Top, from: i, to: i, color: c });
                segments.push(Segment { half: Half::Bottom, from: i, to: i, color: c });
            }
        }

        // Lines going down to the parents.
        for (n, p) in parents.iter().enumerate() {
            let target = match lanes.iter().position(|l| l.as_ref().is_some_and(|(w, _)| w == p)) {
                // A lane further right already waits for our first parent: pull it into ours,
                // so the leftmost branch (usually main) stays straight.
                Some(j) if n == 0 && j > lane => {
                    if let Some(s) = segments.iter_mut().find(|s| s.half == Half::Bottom && s.from == j && s.to == j) {
                        s.to = lane;
                    }
                    lanes[j] = None;
                    lanes[lane] = Some((p.clone(), color));
                    lane
                }
                Some(j) => j,
                None => {
                    // First parent continues this lane in the same color; merged parents get a new lane.
                    let (j, c) = if n == 0 { (lane, color) } else { (free_slot(&mut lanes), distinct(&lanes, color_of(p))) };
                    lanes[j] = Some((p.clone(), c));
                    j
                }
            };
            // The first-parent line belongs to this commit's branch, a merge line to the merged branch.
            let c = if n == 0 { color } else { lanes[target].as_ref().unwrap().1 };
            segments.push(Segment { half: Half::Bottom, from: lane, to: target, color: c });
        }

        // Drop trailing empty lanes so the graph doesn't stay wide forever.
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        rows.push(Row { lane, color, segments });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(i: &usize) -> usize {
        *i
    }

    fn seg(half: Half, from: usize, to: usize, color: usize) -> Segment {
        Segment { half, from, to, color }
    }

    #[test]
    fn linear_history_stays_in_one_lane() {
        let rows = layout(&[(3, vec![2]), (2, vec![1]), (1, vec![])], id);
        assert!(rows.iter().all(|r| r.lane == 0 && r.color == 3));
        assert_eq!(rows[2].segments, vec![seg(Half::Top, 0, 0, 3)]);
    }

    #[test]
    fn merge_opens_and_closes_a_lane() {
        // 4 = merge(3, 2); 3 and 2 both have parent 1.
        let rows = layout(&[(4, vec![3, 2]), (3, vec![1]), (2, vec![1]), (1, vec![])], id);
        assert_eq!((rows[0].lane, rows[1].lane, rows[2].lane, rows[3].lane), (0, 0, 1, 0));
        // The first parent keeps the lane color, the merged parent gets its own.
        assert_eq!(rows[0].segments, vec![seg(Half::Bottom, 0, 0, 4), seg(Half::Bottom, 0, 1, 2)]);
        assert_eq!(rows[2].color, 2);
        // 2's line bends back into lane 0 (already waiting for 1), lane 1 closes at commit 1.
        assert!(rows[2].segments.contains(&seg(Half::Bottom, 1, 0, 2)));
        assert_eq!(rows[3].segments, vec![seg(Half::Top, 0, 0, 4)]);
    }

    #[test]
    fn shared_parent_stays_in_leftmost_lane() {
        // Branch commit 2 is listed before main's 3, both have parent 1.
        let rows = layout(&[(4, vec![3, 2]), (2, vec![1]), (3, vec![1]), (1, vec![])], id);
        assert_eq!(rows[1].lane, 1);
        assert!(rows[2].segments.contains(&seg(Half::Bottom, 1, 0, 2)));
        assert!(rows[2].segments.contains(&seg(Half::Bottom, 0, 0, 4)));
        assert_eq!((rows[3].lane, rows[3].color), (0, 4));
    }

    #[test]
    fn side_by_side_lanes_never_share_a_color() {
        let rows = layout(&[(3, vec![1]), (2, vec![1]), (1, vec![])], |_| 7);
        assert_eq!((rows[0].color, rows[1].color), (7, 8));
    }

    #[test]
    fn two_branch_tips_get_two_colors() {
        let rows = layout(&[(3, vec![1]), (2, vec![1]), (1, vec![])], id);
        assert_eq!((rows[0].color, rows[1].color), (3, 2));
        assert_eq!(rows[1].lane, 1);
    }
}
