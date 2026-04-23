#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VendorKind {
    Claude,
    Codex,
    Gemini,
    Kimi,
}

#[derive(Debug, Clone)]
pub struct QuotaError {
    pub vendor: VendorKind,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ModelStatus {
    pub vendor: VendorKind,
    pub name: String,
    pub stupid_level: Option<u8>,
    pub quota_percent: Option<u8>,
    pub idea_rank: u8,
    pub planning_rank: u8,
    pub build_rank: u8,
    pub review_rank: u8,
    pub idea_weight: f64,
    pub planning_weight: f64,
    pub build_weight: f64,
    pub review_weight: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Planning,
    Build,
    Review,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub vendor: VendorKind,
    pub name: String,
    pub stupid_level: Option<u8>,
    pub quota_percent: Option<u8>,
    pub overall_score: f64,
    pub display_order: usize,
    pub idea_probability: f64,
    pub planning_probability: f64,
    pub build_probability: f64,
    pub review_probability: f64,
}

impl ModelStatus {
    pub fn rank_for(&self, task: TaskKind) -> u8 {
        match task {
            TaskKind::Planning => self.planning_rank,
            TaskKind::Build => self.build_rank,
            TaskKind::Review => self.review_rank,
        }
    }
}
