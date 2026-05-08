use std::fmt;

pub const PLAN_SCHEMA_V1_MARKER: &str = "<!-- plan-schema: v1 -->";

const TOP_LEVEL_SECTIONS: [&str; 4] = [
    "Goal Description",
    "Acceptance Criteria",
    "Path Boundaries",
    "Dependencies and Sequence",
];

const PATH_BOUNDARIES_SUBSECTIONS: [&str; 3] = [
    "Upper Bound (Maximum Scope)",
    "Lower Bound (Minimum Scope)",
    "Allowed Choices",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanSchemaIssue {
    MissingSection { section: String },
    OutOfOrderSection { section: String },
    MissingAcBlock,
    MissingPositiveTestsBullet { ac: String },
    MissingNegativeTestsBullet { ac: String },
    MissingAllowedChoicesLine { line: String },
}

impl fmt::Display for PlanSchemaIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSection { section } => write!(f, "missing-section: {section}"),
            Self::OutOfOrderSection { section } => write!(f, "out-of-order-section: {section}"),
            Self::MissingAcBlock => write!(f, "missing-ac-block"),
            Self::MissingPositiveTestsBullet { ac } => {
                write!(f, "missing-positive-tests-bullet: {ac}")
            }
            Self::MissingNegativeTestsBullet { ac } => {
                write!(f, "missing-negative-tests-bullet: {ac}")
            }
            Self::MissingAllowedChoicesLine { line } => {
                write!(f, "missing-allowed-choices-line: {line}")
            }
        }
    }
}

pub fn validate_plan_schema(text: &str) -> Result<(), Vec<PlanSchemaIssue>> {
    let lines = text.lines().collect::<Vec<_>>();
    let mut issues = validate_top_level_sections(&lines);
    issues.extend(validate_acceptance_criteria(&lines));
    issues.extend(validate_path_boundaries(&lines));
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

fn validate_top_level_sections(lines: &[&str]) -> Vec<PlanSchemaIssue> {
    let headings = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            line.trim_start()
                .strip_prefix("## ")
                .map(|heading| (index, heading.trim()))
        })
        .collect::<Vec<_>>();

    let mut issues = Vec::new();
    let mut previous_index = None;

    for section in TOP_LEVEL_SECTIONS {
        let position = headings
            .iter()
            .find(|(_, heading)| *heading == section)
            .map(|(index, _)| *index);

        match position {
            Some(index) => {
                if previous_index.is_some_and(|previous| index < previous) {
                    issues.push(PlanSchemaIssue::OutOfOrderSection {
                        section: section.to_string(),
                    });
                } else {
                    previous_index = Some(index);
                }
            }
            None => issues.push(PlanSchemaIssue::MissingSection {
                section: format!("## {section}"),
            }),
        }
    }

    issues
}

fn validate_acceptance_criteria(lines: &[&str]) -> Vec<PlanSchemaIssue> {
    let Some((start, end)) = section_bounds(lines, "Acceptance Criteria") else {
        return Vec::new();
    };

    let mut issues = Vec::new();
    let mut current_ac: Option<String> = None;
    let mut has_positive = false;
    let mut has_negative = false;
    let mut saw_any_ac = false;

    let finalize_ac = |issues: &mut Vec<PlanSchemaIssue>,
                       current_ac: &Option<String>,
                       has_positive: bool,
                       has_negative: bool| {
        if let Some(ac) = current_ac {
            if !has_positive {
                issues.push(PlanSchemaIssue::MissingPositiveTestsBullet { ac: ac.clone() });
            }
            if !has_negative {
                issues.push(PlanSchemaIssue::MissingNegativeTestsBullet { ac: ac.clone() });
            }
        }
    };

    for line in &lines[start + 1..end] {
        let trimmed = line.trim_start();
        if let Some(ac_title) = parse_ac_title(trimmed) {
            saw_any_ac = true;
            finalize_ac(&mut issues, &current_ac, has_positive, has_negative);
            current_ac = Some(ac_title);
            has_positive = false;
            has_negative = false;
            continue;
        }

        if current_ac.is_some() {
            if trimmed.starts_with("- Positive Tests (expected to PASS):") {
                has_positive = true;
            } else if trimmed.starts_with("- Negative Tests (expected to FAIL):") {
                has_negative = true;
            }
        }
    }

    finalize_ac(&mut issues, &current_ac, has_positive, has_negative);

    if !saw_any_ac {
        issues.push(PlanSchemaIssue::MissingAcBlock);
    }

    issues
}

fn validate_path_boundaries(lines: &[&str]) -> Vec<PlanSchemaIssue> {
    let Some((start, end)) = section_bounds(lines, "Path Boundaries") else {
        return Vec::new();
    };

    let subsection_headings = lines[start + 1..end]
        .iter()
        .enumerate()
        .filter_map(|(offset, line)| {
            line.trim_start()
                .strip_prefix("### ")
                .map(|heading| (start + 1 + offset, heading.trim()))
        })
        .collect::<Vec<_>>();

    let mut issues = Vec::new();
    for subsection in PATH_BOUNDARIES_SUBSECTIONS {
        if subsection_headings
            .iter()
            .all(|(_, heading)| *heading != subsection)
        {
            issues.push(PlanSchemaIssue::MissingSection {
                section: format!("### {subsection}"),
            });
        }
    }

    if let Some((allowed_start, allowed_end)) =
        subsection_bounds(&subsection_headings, "Allowed Choices", end)
    {
        let mut has_can_use = false;
        let mut has_cannot_use = false;
        for line in &lines[allowed_start + 1..allowed_end] {
            let trimmed = line.trim();
            let normalized = trimmed.strip_prefix("- ").unwrap_or(trimmed).trim_start();
            if normalized.starts_with("Can use:") {
                has_can_use = true;
            } else if normalized.starts_with("Cannot use:") {
                has_cannot_use = true;
            }
        }
        if !has_can_use {
            issues.push(PlanSchemaIssue::MissingAllowedChoicesLine {
                line: "Can use:".to_string(),
            });
        }
        if !has_cannot_use {
            issues.push(PlanSchemaIssue::MissingAllowedChoicesLine {
                line: "Cannot use:".to_string(),
            });
        }
    }

    issues
}

fn section_bounds(lines: &[&str], heading: &str) -> Option<(usize, usize)> {
    let start = lines
        .iter()
        .position(|line| line.trim_start() == format!("## {heading}"))?;
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.trim_start().starts_with("## "))
        .map(|offset| start + 1 + offset)
        .unwrap_or(lines.len());
    Some((start, end))
}

fn subsection_bounds(
    headings: &[(usize, &str)],
    heading: &str,
    section_end: usize,
) -> Option<(usize, usize)> {
    let position = headings
        .iter()
        .position(|(_, candidate)| *candidate == heading)?;
    let start = headings[position].0;
    let end = headings
        .get(position + 1)
        .map(|(index, _)| *index)
        .unwrap_or(section_end);
    Some((start, end))
}

fn parse_ac_title(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix("- AC-")?;
    let (number, title) = rest.split_once(':')?;
    if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(format!("AC-{number}:{}", title))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_plan() -> String {
        r#"<!-- plan-schema: v1 -->
# Example Plan

## Goal Description
Ship the feature.

## Acceptance Criteria
- AC-1: cover the main path
  - Positive Tests (expected to PASS):
    - accepts a valid input
  - Negative Tests (expected to FAIL):
    - rejects an invalid input

## Path Boundaries

### Upper Bound (Maximum Scope)
Enough to satisfy the full task.

### Lower Bound (Minimum Scope)
Enough to ship a minimal slice.

### Allowed Choices
- Can use: existing helpers
- Cannot use: new third-party crates

## Dependencies and Sequence
1. Milestone 1: implement the feature.
"#
        .to_string()
    }

    #[test]
    fn accepts_minimal_valid_plan() {
        assert_eq!(validate_plan_schema(&valid_plan()), Ok(()));
    }

    #[test]
    fn rejects_missing_goal_description_section() {
        let plan = valid_plan().replace("## Goal Description\nShip the feature.\n\n", "");
        assert_eq!(
            validate_plan_schema(&plan),
            Err(vec![PlanSchemaIssue::MissingSection {
                section: "## Goal Description".to_string()
            }])
        );
    }

    #[test]
    fn rejects_missing_acceptance_criteria_section() {
        let plan = valid_plan().replace(
            "## Acceptance Criteria\n- AC-1: cover the main path\n  - Positive Tests (expected to PASS):\n    - accepts a valid input\n  - Negative Tests (expected to FAIL):\n    - rejects an invalid input\n\n",
            "",
        );
        assert_eq!(
            validate_plan_schema(&plan),
            Err(vec![PlanSchemaIssue::MissingSection {
                section: "## Acceptance Criteria".to_string()
            }])
        );
    }

    #[test]
    fn rejects_missing_path_boundaries_section() {
        let plan = valid_plan().replace(
            "## Path Boundaries\n\n### Upper Bound (Maximum Scope)\nEnough to satisfy the full task.\n\n### Lower Bound (Minimum Scope)\nEnough to ship a minimal slice.\n\n### Allowed Choices\n- Can use: existing helpers\n- Cannot use: new third-party crates\n\n",
            "",
        );
        assert_eq!(
            validate_plan_schema(&plan),
            Err(vec![PlanSchemaIssue::MissingSection {
                section: "## Path Boundaries".to_string()
            }])
        );
    }

    #[test]
    fn rejects_missing_dependencies_and_sequence_section() {
        let plan = valid_plan().replace(
            "## Dependencies and Sequence\n1. Milestone 1: implement the feature.\n",
            "",
        );
        assert_eq!(
            validate_plan_schema(&plan),
            Err(vec![PlanSchemaIssue::MissingSection {
                section: "## Dependencies and Sequence".to_string()
            }])
        );
    }

    #[test]
    fn rejects_out_of_order_sections() {
        let plan = valid_plan().replace(
            "## Goal Description\nShip the feature.\n\n## Acceptance Criteria",
            "## Acceptance Criteria",
        );
        let plan = plan.replace(
            "## Path Boundaries",
            "## Goal Description\nShip the feature.\n\n## Path Boundaries",
        );
        assert_eq!(
            validate_plan_schema(&plan),
            Err(vec![PlanSchemaIssue::OutOfOrderSection {
                section: "Acceptance Criteria".to_string()
            }])
        );
    }

    #[test]
    fn rejects_missing_ac_block() {
        let plan = valid_plan().replace(
            "- AC-1: cover the main path\n  - Positive Tests (expected to PASS):\n    - accepts a valid input\n  - Negative Tests (expected to FAIL):\n    - rejects an invalid input\n",
            "",
        );
        assert_eq!(
            validate_plan_schema(&plan),
            Err(vec![PlanSchemaIssue::MissingAcBlock])
        );
    }

    #[test]
    fn rejects_missing_positive_tests_bullet() {
        let plan = valid_plan().replace(
            "  - Positive Tests (expected to PASS):\n    - accepts a valid input\n",
            "",
        );
        assert_eq!(
            validate_plan_schema(&plan),
            Err(vec![PlanSchemaIssue::MissingPositiveTestsBullet {
                ac: "AC-1: cover the main path".to_string()
            }])
        );
    }

    #[test]
    fn rejects_missing_negative_tests_bullet() {
        let plan = valid_plan().replace(
            "  - Negative Tests (expected to FAIL):\n    - rejects an invalid input\n",
            "",
        );
        assert_eq!(
            validate_plan_schema(&plan),
            Err(vec![PlanSchemaIssue::MissingNegativeTestsBullet {
                ac: "AC-1: cover the main path".to_string()
            }])
        );
    }

    #[test]
    fn rejects_missing_cannot_use_line() {
        let plan = valid_plan().replace("- Cannot use: new third-party crates\n", "");
        assert_eq!(
            validate_plan_schema(&plan),
            Err(vec![PlanSchemaIssue::MissingAllowedChoicesLine {
                line: "Cannot use:".to_string()
            }])
        );
    }
}
