use std::time::Duration;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    netease::Song,
    tui::{
        LibraryFocus, MediaLibrary, centered_popup, draw_library_source_message,
        fitted_unselected_library_row, library_row, panel_border_style, separation_spinner_frame,
        wrapped_list_scroll,
    },
};

use super::{LoginJob, NeteaseModal, NeteasePanel, NeteaseView};

pub(in crate::tui) fn draw_netease_sources(frame: &mut Frame, area: Rect, library: &MediaLibrary) {
    let Some(panel) = &library.netease else {
        return;
    };
    let status_height = if library.netease_message.is_some() {
        6
    } else {
        0
    };
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(status_height)])
        .split(area);
    let list_area = areas[0];
    let rows = if panel.chrome_login_job.is_some() {
        vec![Line::from(format!(
            "{} Importing login from Chrome...",
            separation_spinner_frame()
        ))]
    } else if !panel.is_logged_in() {
        vec![Line::from(netease_sign_in_hint())]
    } else if panel.catalog_job.is_some() {
        vec![Line::from(format!(
            "{} Loading songs...",
            separation_spinner_frame()
        ))]
    } else {
        let row_width = usize::from(list_area.width.saturating_sub(2));
        panel
            .visible_songs()
            .iter()
            .enumerate()
            .map(|(row, song)| {
                let label = netease_song_label(song, panel.selected_ids.contains(&song.id));
                let selected = row == panel.selected_row;
                if panel.view == NeteaseView::Liked && !selected {
                    fitted_unselected_library_row(&label, row_width)
                } else {
                    library_row(&label, selected, library.focus == LibraryFocus::Sources)
                }
            })
            .collect::<Vec<_>>()
    };
    let title_view = match panel.view {
        NeteaseView::Liked => "Liked Songs",
        NeteaseView::Search => "Search",
    };
    let task = panel.download_job.as_ref().map_or_else(
        || format!("{} selected", panel.selected_ids.len()),
        |job| {
            format!(
                "downloading {} · {} queued",
                job.song.title,
                panel.download_queue.len()
            )
        },
    );
    let title = if library.focus == LibraryFocus::Sources {
        format!("▶ NetEase · {title_view} · {task} · n Local")
    } else {
        format!("NetEase · {title_view} · {task}")
    };
    let scroll = wrapped_list_scroll(&rows, panel.selected_row, list_area.width, list_area.height);
    frame.render_widget(
        Paragraph::new(if rows.is_empty() {
            vec![Line::from("No songs")]
        } else {
            rows
        })
        .block(
            Block::default()
                .title(format!(" {title} "))
                .borders(Borders::ALL)
                .border_style(panel_border_style(Some(
                    library.focus == LibraryFocus::Sources,
                ))),
        )
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false }),
        list_area,
    );
    if let Some(message) = &library.netease_message {
        draw_library_source_message(frame, areas[1], message, message.is_error());
    }
}

pub(in crate::tui) fn netease_song_label(song: &Song, selected: bool) -> String {
    let checked = if selected { "[x]" } else { "[ ]" };
    let availability = if song.available {
        ""
    } else {
        " · unavailable (account or region)"
    };
    format!(
        "{checked} {}-{} · {} · {}{availability}",
        song.primary_artist(),
        song.title,
        song.album,
        song.max_quality
    )
}

pub(in crate::tui) fn netease_sign_in_hint() -> &'static str {
    "Press c to import Chrome login · i for QR"
}

pub(in crate::tui) fn netease_download_busy_hint() -> &'static str {
    "Wait for the current NetEase download queue to finish"
}

pub(in crate::tui) fn netease_session_expired_hint() -> &'static str {
    "NetEase session expired · press c to import Chrome login · i for QR"
}

pub(in crate::tui) fn draw_netease_modal(frame: &mut Frame, library: &MediaLibrary) {
    if library.confirm_exit {
        draw_exit_confirmation(frame, library);
        return;
    }
    let Some(panel) = &library.netease else {
        return;
    };
    let Some(modal) = &panel.modal else {
        return;
    };
    match modal {
        NeteaseModal::Risk => draw_netease_risk(frame),
        NeteaseModal::Login(job) => draw_netease_login(frame, job),
        NeteaseModal::Search(query) => draw_netease_search(frame, query),
        NeteaseModal::ConfirmDownload => {
            draw_netease_download_confirmation(frame, panel.selected_ids.len());
        }
    }
}

pub(in crate::tui) fn draw_exit_confirmation(frame: &mut Frame, library: &MediaLibrary) {
    let separation_active = library.job.is_some() || !library.queue.is_empty();
    let downloads_active = library
        .netease
        .as_ref()
        .is_some_and(NeteasePanel::active_download);
    let prompt = match (separation_active, downloads_active) {
        (true, true) => {
            "NetEase downloads and separation tasks are still active. Cancel them and exit?"
        }
        (true, false) => "Separation tasks are still active. Cancel them and exit?",
        (false, true) => "NetEase downloads are still active. Cancel them and exit?",
        (false, false) => "Exit K3?",
    };
    let popup = centered_popup(frame.area(), 64, 5);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(format!("{prompt}\n\ny yes · n no"))
            .block(
                Block::default()
                    .title(" Confirm exit ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

pub(in crate::tui) fn draw_netease_risk(frame: &mut Frame) {
    let popup = centered_popup(frame.area(), 88, 11);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(
            "NetEase is an experimental source that uses undocumented web endpoints.\n\
             It may stop working without notice. Only download music your account is allowed\n\
             to access, and follow applicable terms and local rules. Credentials are stored\n\
             in a private local K3 file.\n\nAccept and continue? · y yes · n no",
        )
        .block(
            Block::default()
                .title(" Experimental NetEase source ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false }),
        popup,
    );
}

pub(in crate::tui) fn draw_netease_login(frame: &mut Frame, job: &LoginJob) {
    let qr_width = job
        .qr_lines
        .iter()
        .map(|line| UnicodeWidthStr::width(line.as_str()))
        .max()
        .unwrap_or(1);
    let width = u16::try_from(qr_width.saturating_add(4))
        .unwrap_or(u16::MAX)
        .clamp(24, frame.area().width.saturating_sub(2).max(1));
    let height = u16::try_from(job.qr_lines.len().saturating_add(4))
        .unwrap_or(u16::MAX)
        .clamp(5, frame.area().height.saturating_sub(2).max(1));
    let popup = centered_popup(frame.area(), width, height);
    let mut lines = job
        .qr_lines
        .iter()
        .cloned()
        .map(Line::from)
        .collect::<Vec<_>>();
    let remaining = Duration::from_mins(3).saturating_sub(job.started.elapsed());
    let status = if job.status.contains("expired") {
        job.status.clone()
    } else {
        format!("{} · expires in {}s", job.status, remaining.as_secs())
    };
    lines.push(Line::from(status));
    lines.push(Line::from("c import Chrome · r refresh · Esc cancel"));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" Scan with NetEase Cloud Music ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

pub(in crate::tui) fn draw_netease_search(frame: &mut Frame, query: &str) {
    let popup = centered_popup(frame.area(), 80, 3);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(query).block(
            Block::default()
                .title(" Search NetEase songs · Enter search · Esc cancel ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        ),
        popup,
    );
    let cursor = u16::try_from(UnicodeWidthStr::width(query)).unwrap_or(u16::MAX);
    frame.set_cursor_position((
        popup.x + 1 + cursor.min(popup.width.saturating_sub(2)),
        popup.y + 1,
    ));
}

pub(in crate::tui) fn draw_netease_download_confirmation(frame: &mut Frame, count: usize) {
    let popup = centered_popup(frame.area(), 72, 7);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(format!(
            "Download {count} selected songs at the highest available quality?\n\n\
             y start · n cancel"
        ))
        .block(
            Block::default()
                .title(" Confirm NetEase download ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false }),
        popup,
    );
}
