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

#[derive(Clone)]
pub enum Layout {
    Leaf(usize),
    Split(SplitDir, Box<Layout>, Box<Layout>),
}

impl Layout {
    pub fn compute(&self, area: Rect, out: &mut Vec<(usize, Rect)>) {
        match self {
            Layout::Leaf(idx) => out.push((*idx, area)),
            Layout::Split(dir, a, b) => {
                let (ra, rb) = split_area(area, *dir);
                a.compute(ra, out);
                b.compute(rb, out);
            }
        }
    }

    pub fn split_at(&mut self, target: usize, new_idx: usize, dir: SplitDir) -> bool {
        match self {
            Layout::Leaf(i) if *i == target => {
                let old = *i;
                *self = Layout::Split(
                    dir,
                    Box::new(Layout::Leaf(old)),
                    Box::new(Layout::Leaf(new_idx)),
                );
                true
            }
            Layout::Split(_, a, b) => {
                a.split_at(target, new_idx, dir) || b.split_at(target, new_idx, dir)
            }
            _ => false,
        }
    }

    pub fn close_at(&mut self, target: usize) -> bool {
        match self {
            Layout::Leaf(_) => false,
            Layout::Split(_, a, b) => {
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
            Layout::Split(_, a, b) => {
                a.renumber(removed);
                b.renumber(removed);
            }
        }
    }
}

fn split_area(area: Rect, dir: SplitDir) -> (Rect, Rect) {
    match dir {
        SplitDir::Vertical => {
            let half = area.width / 2;
            let a = Rect::new(area.x, area.y, half, area.height);
            let b = Rect::new(area.x + half, area.y, area.width - half, area.height);
            (a, b)
        }
        SplitDir::Horizontal => {
            let half = area.height / 2;
            let a = Rect::new(area.x, area.y, area.width, half);
            let b = Rect::new(area.x, area.y + half, area.width, area.height - half);
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
