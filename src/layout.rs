use ratatui::layout::Rect;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SplitDir {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy, Debug)]
pub enum NavDir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug)]
pub enum ResizeDir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone)]
pub enum Layout {
    Leaf(usize),
    Split {
        dir: SplitDir,
        a: Box<Layout>,
        b: Box<Layout>,
        ratio: f32,
    },
}

impl Layout {
    pub fn new_split(dir: SplitDir, a: Layout, b: Layout) -> Self {
        Layout::Split {
            dir,
            a: Box::new(a),
            b: Box::new(b),
            ratio: 0.5,
        }
    }

    pub fn compute(&self, area: Rect, out: &mut Vec<(usize, Rect)>) {
        match self {
            Layout::Leaf(idx) => out.push((*idx, area)),
            Layout::Split { dir, a, b, ratio } => {
                let (ra, rb) = split_area_ratio(area, *dir, *ratio);
                a.compute(ra, out);
                b.compute(rb, out);
            }
        }
    }

    pub fn split_at(&mut self, target: usize, new_idx: usize, dir: SplitDir) -> bool {
        match self {
            Layout::Leaf(i) if *i == target => {
                let old = *i;
                *self = Layout::new_split(dir, Layout::Leaf(old), Layout::Leaf(new_idx));
                true
            }
            Layout::Split { a, b, .. } => {
                a.split_at(target, new_idx, dir) || b.split_at(target, new_idx, dir)
            }
            _ => false,
        }
    }

    pub fn close_at(&mut self, target: usize) -> bool {
        match self {
            Layout::Leaf(_) => false,
            Layout::Split { a, b, .. } => {
                if let Layout::Leaf(i) = **a {
                    if i == target {
                        let sibling = std::mem::replace(b.as_mut(), Layout::Leaf(0));
                        *self = sibling;
                        return true;
                    }
                }
                if let Layout::Leaf(i) = **b {
                    if i == target {
                        let sibling = std::mem::replace(a.as_mut(), Layout::Leaf(0));
                        *self = sibling;
                        return true;
                    }
                }
                a.close_at(target) || b.close_at(target)
            }
        }
    }

    pub fn renumber(&mut self, removed: usize) {
        match self {
            Layout::Leaf(i) => {
                if *i > removed {
                    *i -= 1;
                }
            }
            Layout::Split { a, b, .. } => {
                a.renumber(removed);
                b.renumber(removed);
            }
        }
    }

    /// Меняет ratio ближайшего сплита (в направлении `dir`), содержащего `target`.
    pub fn adjust_ratio(&mut self, target: usize, dir: ResizeDir, delta: f32) -> bool {
        match self {
            Layout::Leaf(_) => false,
            Layout::Split {
                dir: split_dir,
                a,
                b,
                ratio,
            } => {
                let target_in_a = a.contains(target);
                let target_in_b = b.contains(target);

                if !target_in_a && !target_in_b {
                    return false;
                }

                let matches_dir = match (split_dir, dir) {
                    (SplitDir::Vertical, ResizeDir::Left | ResizeDir::Right) => true,
                    (SplitDir::Horizontal, ResizeDir::Up | ResizeDir::Down) => true,
                    _ => false,
                };

                if matches_dir && (a.is_leaf(target) || b.is_leaf(target)) {
                    let sign = match (dir, target_in_a) {
                        (ResizeDir::Right, true) | (ResizeDir::Left, false) => 1.0,
                        (ResizeDir::Left, true) | (ResizeDir::Right, false) => -1.0,
                        (ResizeDir::Down, true) | (ResizeDir::Up, false) => 1.0,
                        (ResizeDir::Up, true) | (ResizeDir::Down, false) => -1.0,
                    };
                    *ratio = (*ratio + sign * delta).clamp(0.1, 0.9);
                    return true;
                }

                if target_in_a {
                    a.adjust_ratio(target, dir, delta)
                } else {
                    b.adjust_ratio(target, dir, delta)
                }
            }
        }
    }

    fn contains(&self, target: usize) -> bool {
        match self {
            Layout::Leaf(i) => *i == target,
            Layout::Split { a, b, .. } => a.contains(target) || b.contains(target),
        }
    }

    fn is_leaf(&self, target: usize) -> bool {
        matches!(self, Layout::Leaf(i) if *i == target)
    }
}

fn split_area_ratio(area: Rect, dir: SplitDir, ratio: f32) -> (Rect, Rect) {
    let ratio = ratio.clamp(0.1, 0.9);
    match dir {
        SplitDir::Vertical => {
            let w1 = ((area.width as f32 * ratio) as u16)
                .max(1)
                .min(area.width.saturating_sub(1));
            let a = Rect::new(area.x, area.y, w1, area.height);
            let b = Rect::new(area.x + w1, area.y, area.width - w1, area.height);
            (a, b)
        }
        SplitDir::Horizontal => {
            let h1 = ((area.height as f32 * ratio) as u16)
                .max(1)
                .min(area.height.saturating_sub(1));
            let a = Rect::new(area.x, area.y, area.width, h1);
            let b = Rect::new(area.x, area.y + h1, area.width, area.height - h1);
            (a, b)
        }
    }
}

pub fn find_neighbor(rects: &[(usize, Rect)], focused: usize, dir: NavDir) -> Option<usize> {
    let focused_rect = rects.iter().find(|(i, _)| *i == focused)?.1;
    let fx = focused_rect.x as i32 + focused_rect.width as i32 / 2;
    let fy = focused_rect.y as i32 + focused_rect.height as i32 / 2;

    let mut best: Option<(usize, i32)> = None;

    for (idx, rect) in rects {
        if *idx == focused {
            continue;
        }
        let cx = rect.x as i32 + rect.width as i32 / 2;
        let cy = rect.y as i32 + rect.height as i32 / 2;
        let dx = cx - fx;
        let dy = cy - fy;

        let dist = match dir {
            NavDir::Left if dx < 0 => dx.abs() * 4 + dy.abs(),
            NavDir::Right if dx > 0 => dx * 4 + dy.abs(),
            NavDir::Up if dy < 0 => dy.abs() * 4 + dx.abs(),
            NavDir::Down if dy > 0 => dy * 4 + dx.abs(),
            _ => continue,
        };

        if best.map_or(true, |(_, bd)| dist < bd) {
            best = Some((*idx, dist));
        }
    }

    best.map(|(i, _)| i)
}
