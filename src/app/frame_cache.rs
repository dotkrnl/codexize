pub(crate) fn cached_header_y_offsets<F>(populate: F) -> (Vec<usize>, usize)
where
    F: FnOnce() -> (Vec<usize>, usize),
{
    populate()
}

pub(crate) fn cached_running_depth_0_header<F>(populate: F) -> Option<(usize, usize)>
where
    F: FnOnce() -> Option<(usize, usize)>,
{
    populate()
}
