use super::*;

impl SessionPicker {
    pub(super) fn draw(&mut self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        let term_h = area.height;
        let width = area.width;
        let degenerate = term_h < 10;

        let top_rule_right = if self.show_archived {
            Some("showing archived".to_string())
        } else {
            None
        };
        let top_rule = top_rule_with_left_spans(
            vec![Span::styled(
                "Sessions",
                Style::default().fg(Color::DarkGray),
            )],
            top_rule_right.as_deref(),
            width,
        );

        let status_content = if degenerate {
            None
        } else {
            self.status_line.render()
        };
        let status_h = if status_content.is_some() { 1 } else { 0 };

        let footer_h = if self.input_mode {
            let inner_width = width.saturating_sub(4).max(1) as usize;
            let wrapped_len = wrap_input(&self.input_buffer, inner_width).len().max(1) as u16;
            wrapped_len + 2
        } else {
            1 + status_h
        };

        let chrome_h = 1 + 1 + footer_h;
        let body_h = term_h.saturating_sub(chrome_h);
        self.body_inner_height = body_h as usize;

        let mut y = area.y;

        let top_rect = Rect::new(area.x, y, width, 1);
        frame.render_widget(Paragraph::new(vec![top_rule]), top_rect);
        y += 1;

        if body_h > 0 {
            let body_rect = Rect::new(area.x, y, width, body_h);
            self.draw_list(frame, body_rect, degenerate);
            y += body_h;
        }

        if !self.input_mode {
            let bottom_rule = bottom_rule(width, None);
            let bottom_rect = Rect::new(area.x, y, width, 1);
            frame.render_widget(Paragraph::new(vec![bottom_rule]), bottom_rect);
            y += 1;
        }

        if let Some(kind) = self.confirm_modal {
            self.draw_modal(frame, area, kind);
        } else if self.input_mode {
            // Reuse the chrome divider row so bottom_sheet supplies the only input divider.
            let remain = area.height.saturating_sub(y - area.y);
            let input_rect = Rect::new(area.x, y, width, remain);
            self.draw_input(frame, input_rect);
        } else {
            let remain = area.height.saturating_sub(y - area.y);
            if remain > 0 {
                let footer_rect = Rect::new(area.x, y, width, remain);
                self.draw_footer(frame, footer_rect, degenerate);
            }
        }

        if self.palette.open && area.height > 0 && area.width > 0 && self.confirm_modal.is_none() {
            let overlay_h = self.palette_overlay_height(area.height);
            if overlay_h > 0 {
                let overlay = Rect::new(
                    area.x,
                    area.y + area.height.saturating_sub(overlay_h),
                    area.width,
                    overlay_h,
                );
                frame.render_widget(Clear, overlay);
                let lines = self.palette_lines(width, overlay_h);
                frame.render_widget(Paragraph::new(lines), overlay);
            }
        }
    }

    fn draw_list(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
        area: ratatui::layout::Rect,
        degenerate: bool,
    ) {
        let visible = self.visible_entries();

        if visible.is_empty() {
            let message = Paragraph::new("No sessions yet — press n to create one")
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(
                message,
                Rect::new(
                    area.x + area.width.saturating_sub(41) / 2,
                    area.y + area.height / 2,
                    area.width.min(41),
                    1,
                ),
            );
            return;
        }

        let now = SystemTime::now();
        let mut rendered_rows: Vec<(usize, Line<'static>)> = Vec::new();

        let mut selected_top_idx = 0;
        let mut selected_bottom_idx = 0;

        for (idx, entry) in visible.iter().enumerate() {
            let (badge, color, prefix) = phase_badge(entry.current_phase);
            let time = format_relative_time(entry.last_modified, now);

            let is_selected = idx == self.selected;
            let is_expanded = self.expanded.as_ref() == Some(&entry.session_id) && !degenerate;

            let leading = if is_selected { ">" } else { " " };
            let mut spans = vec![
                Span::raw(leading),
                Span::styled(prefix, Style::default().fg(color)),
                Span::raw(" "),
                Span::styled(format!("{:<12}", badge), Style::default().fg(color)),
            ];
            for label in mode_badge_labels(entry.modes) {
                let style = match label {
                    "[YOLO]" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    "[CHEAP]" => Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                    _ => Style::default().fg(Color::DarkGray),
                };
                spans.push(Span::raw(" "));
                spans.push(Span::styled(label, style));
            }
            spans.extend([
                Span::raw("  "),
                Span::styled(format!("{:<8}", time), Style::default().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::raw(entry.idea_summary.clone()),
            ]);
            let mut line = Line::from(spans);
            if is_selected {
                for span in &mut line.spans {
                    span.style = span.style.add_modifier(Modifier::BOLD);
                }
            }

            if is_selected {
                selected_top_idx = rendered_rows.len();
            }
            rendered_rows.push((idx, line));

            if is_expanded {
                let mut details = Vec::new();
                let dim = Style::default().fg(Color::DarkGray);
                let default_style = Style::default().fg(Color::White);

                let phase_status = if entry.current_phase == Phase::Done {
                    "done"
                } else if entry.current_phase == Phase::BlockedNeedsUser {
                    "blocked"
                } else if entry.current_phase == Phase::IdeaInput {
                    "awaiting input"
                } else {
                    "running"
                };

                details.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled("Phase: ", dim),
                    Span::styled(
                        format!("{} ({})", entry.current_phase.label(), phase_status),
                        default_style,
                    ),
                ]));

                let idea_text = if entry.idea_summary == "(no idea yet)" {
                    "(no idea yet)".to_string()
                } else {
                    let st = SessionState::load(&entry.session_id).ok();
                    st.and_then(|s| s.idea_text.clone())
                        .unwrap_or_else(|| entry.idea_summary.clone())
                };

                let wrap_width = area.width.saturating_sub(14).max(1) as usize;
                let idea_lines = wrap_input(&idea_text, wrap_width);
                for (i, il) in idea_lines.iter().enumerate() {
                    let prefix = if i == 0 { "Idea: " } else { "      " };
                    details.push(Line::from(vec![
                        Span::raw("      "),
                        Span::styled(prefix, dim),
                        Span::styled(il.clone(), default_style),
                    ]));
                }

                let st = SessionState::load(&entry.session_id).ok();
                let last_agent = st
                    .and_then(|s| s.agent_runs.last().map(|r| r.window_name.clone()))
                    .unwrap_or_else(|| "none".to_string());
                details.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled("Last agent: ", dim),
                    Span::styled(last_agent, default_style),
                ]));

                let modified: DateTime<Local> = entry.last_modified.into();
                details.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled("Modified: ", dim),
                    Span::styled(
                        modified.format("%Y-%m-%d %H:%M:%S").to_string(),
                        default_style,
                    ),
                ]));

                for d in details {
                    rendered_rows.push((idx, d));
                }
            }
            if is_selected {
                selected_bottom_idx = rendered_rows.len() - 1;
            }
        }

        if selected_top_idx < self.viewport_top {
            self.viewport_top = selected_top_idx;
        } else if selected_bottom_idx >= self.viewport_top + area.height as usize {
            self.viewport_top = selected_bottom_idx + 1 - area.height as usize;
        }

        let end = (self.viewport_top + area.height as usize).min(rendered_rows.len());
        let buf = frame.buffer_mut();
        for (i, (_, line)) in rendered_rows[self.viewport_top..end].iter().enumerate() {
            buf.set_line(area.x, area.y + i as u16, line, area.width);
        }
    }

    pub(super) fn page_step(&self) -> usize {
        picker_view_model::page_step(self.body_inner_height)
    }

    fn keymap_line(
        &self,
        width: u16,
        caps_fn: &dyn Fn(Option<Capability>) -> bool,
    ) -> Line<'static> {
        let nav = vec![
            KeyBinding {
                glyph: "↑↓",
                action: "move",
                is_primary: false,
                capability: None,
            },
            KeyBinding {
                glyph: "Space",
                action: "expand",
                is_primary: false,
                capability: Some(Capability::Expand),
            },
            KeyBinding {
                glyph: "PgUp/PgDn",
                action: "page",
                is_primary: false,
                capability: None,
            },
        ];
        let actions = vec![
            KeyBinding {
                glyph: "Enter",
                action: "open",
                is_primary: true,
                capability: Some(Capability::Input),
            },
            KeyBinding {
                glyph: "n",
                action: "new",
                is_primary: false,
                capability: None,
            },
        ];
        let system = vec![
            KeyBinding {
                glyph: ":",
                action: "palette",
                is_primary: false,
                capability: None,
            },
            KeyBinding {
                glyph: "Esc",
                action: "quit",
                is_primary: false,
                capability: None,
            },
        ];
        render_keymap_line(&[&nav, &actions, &system], caps_fn, width)
    }

    fn draw_footer(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: ratatui::layout::Rect,
        degenerate: bool,
    ) {
        let caps_fn = |cap: Option<Capability>| -> bool {
            match cap {
                Some(Capability::Expand) => !self.entries.is_empty(),
                Some(Capability::Input) => !self.entries.is_empty(),
                _ => true,
            }
        };

        let km_line = self.keymap_line(area.width, &caps_fn);
        let mut lines = Vec::new();

        if let Some(msg) = self.status_line.render() {
            if degenerate {
                lines.push(msg); // replace keymap with status
            } else {
                lines.push(msg);
                lines.push(km_line);
            }
        } else {
            lines.push(km_line);
        }

        let buf = frame.buffer_mut();
        for (i, line) in lines.iter().rev().enumerate() {
            let y = area.y + area.height.saturating_sub(1 + i as u16);
            if y >= area.y {
                buf.set_line(area.x, y, line, area.width);
            }
        }
    }

    fn draw_input(&self, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
        let inner_width = area.width.saturating_sub(4).max(1) as usize;
        let placeholder = "describe your idea...";
        let (text, text_style) = if self.input_buffer.is_empty() {
            (
                placeholder.to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )
        } else {
            (self.input_buffer.clone(), Style::default().fg(Color::White))
        };

        let mut wrapped = wrap_input(&text, inner_width);
        if wrapped.is_empty() {
            wrapped.push(String::new());
        }

        let cursor_pos = {
            let target = if self.input_buffer.is_empty() {
                0
            } else {
                self.input_cursor.min(self.input_buffer.chars().count())
            };
            let mut acc = 0usize;
            let mut found = (wrapped.len().saturating_sub(1), 0usize);
            for (idx, chunk) in wrapped.iter().enumerate() {
                let chunk_len = chunk.chars().count();
                if target <= acc + chunk_len {
                    found = (idx, target - acc);
                    break;
                }
                acc += chunk_len;
            }
            found
        };

        let mut content = Vec::new();
        for (idx, chunk) in wrapped.iter().enumerate() {
            let show_cursor_here = idx == cursor_pos.0;
            let split_col = if show_cursor_here { cursor_pos.1 } else { 0 };

            if show_cursor_here {
                let byte = chunk
                    .char_indices()
                    .nth(split_col)
                    .map(|(i, _)| i)
                    .unwrap_or(chunk.len());
                let (left, right) = (&chunk[..byte], &chunk[byte..]);
                content.push(Line::from(vec![
                    Span::styled(left.to_string(), text_style),
                    Span::styled(
                        "▌".to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::SLOW_BLINK),
                    ),
                    Span::styled(right.to_string(), text_style),
                ]));
            } else {
                content.push(Line::from(Span::styled(chunk.clone(), text_style)));
            }
        }

        let caps_fn: &dyn Fn(Option<Capability>) -> bool = &|_| true;
        let controls = vec![
            KeyBinding {
                glyph: "Enter",
                action: "create",
                is_primary: true,
                capability: None,
            },
            KeyBinding {
                glyph: "Esc",
                action: "cancel",
                is_primary: false,
                capability: None,
            },
        ];
        let keymap = render_keymap_line(&[&controls], caps_fn, area.width);

        let sheet_lines = bottom_sheet(content, keymap, area.height, area.width);
        for (i, line) in sheet_lines.into_iter().enumerate() {
            if i < area.height as usize {
                frame
                    .buffer_mut()
                    .set_line(area.x, area.y + i as u16, &line, area.width);
            }
        }
    }

    fn draw_modal(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: ratatui::layout::Rect,
        kind: ConfirmKind,
    ) {
        let entry = self
            .selected_entry()
            .map(|e| e.idea_summary.clone())
            .unwrap_or_default();
        let (title, warning) = match kind {
            ConfirmKind::Archive => ("Archive this session?", None),
            ConfirmKind::Delete => (
                "Permanently delete this session?",
                Some("This cannot be undone."),
            ),
        };

        let mut content = vec![Line::from(Span::styled(
            title,
            Style::default().fg(Color::White),
        ))];
        if let Some(w) = warning {
            content.push(Line::from(Span::styled(
                w,
                Style::default().fg(Color::White),
            )));
        }
        content.push(Line::from(""));
        content.push(Line::from(Span::styled(
            format!("\"{}\"", entry),
            Style::default().fg(Color::DarkGray),
        )));
        content.push(Line::from(""));

        let caps_fn: &dyn Fn(Option<Capability>) -> bool = &|_| true;
        let actions = vec![
            KeyBinding {
                glyph: "Enter",
                action: "confirm",
                is_primary: true,
                capability: None,
            },
            KeyBinding {
                glyph: "Esc",
                action: "cancel",
                is_primary: false,
                capability: None,
            },
        ];
        let modal_keymap = render_keymap_line(
            &[&actions],
            caps_fn,
            area.width.clamp(40, 80).saturating_sub(4),
        );

        render_modal_overlay(
            frame,
            area,
            Some(title),
            ratatui::style::Style::default().fg(ratatui::style::Color::Yellow),
            content,
            modal_keymap,
        );
    }

    fn palette_overlay_height(&self, total_height: u16) -> u16 {
        picker_view_model::palette_overlay_height(
            &self.palette.buffer,
            self.selected_entry()
                .map(|entry| entry.archived)
                .unwrap_or(false),
            total_height,
        )
    }

    fn palette_lines(&self, width: u16, inner_h: u16) -> Vec<Line<'static>> {
        picker_view_model::palette_lines(
            &self.palette.buffer,
            self.selected_entry()
                .map(|entry| entry.archived)
                .unwrap_or(false),
            width,
            inner_h,
        )
    }
}
