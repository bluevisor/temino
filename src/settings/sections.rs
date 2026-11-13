//! The page's sub tabs, and which sections each one shows.

use super::*;

/// A sub tab of the page: the bar under the title picks one, and only its
/// sections show, so no one page runs on and on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Section {
    #[default]
    Appearance,
    Terminal,
    Ai,
    /// Hidden where there's no relay to sign in to.
    Remote,
    Keys,
    Updates,
}

impl Section {
    pub const ALL: [Section; 6] =
        [Section::Appearance, Section::Terminal, Section::Ai, Section::Remote, Section::Keys, Section::Updates];

    fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Terminal => "Terminal",
            Section::Ai => "AI",
            Section::Remote => "Remote",
            Section::Keys => "Keys",
            Section::Updates => "Updates",
        }
    }

    /// The tabs there are: Remote only with a relay to sign in to.
    pub fn shown(values: &Values) -> Vec<Section> {
        Section::ALL.into_iter().filter(|&s| s != Section::Remote || values.remote.is_some()).collect()
    }
}

impl Ui<'_> {
    /// The bar of sub tabs, centered in the column; returns where the
    /// sections go.
    pub(super) fn section_bar(&mut self, x: f32, y: f32, width: f32, current: Section, values: &Values) -> f32 {
        let shown = Section::shown(values);
        let labels: Vec<String> = shown.iter().map(|s| s.label().to_owned()).collect();
        let total = self.segments_width(&labels) + 4.0;
        let right = x + ((width + total) / 2.0).min(width);
        let selected = shown.iter().position(|&s| s == current);
        self.segments(right, y, &labels, selected, |index| Action::Section(shown[index]));
        y + CONTROL_HEIGHT + 24.0
    }

    /// The sections of one sub tab; returns where the next thing goes.
    pub(super) fn sections(&mut self, x: f32, mut y: f32, width: f32, section: Section, values: &Values) -> f32 {
        match section {
            Section::Appearance => {
                let mut rows = vec![Row::Theme, Row::Mode, Row::ThemeFolder, Row::Background];
                if values.see_through {
                    rows.extend([Row::Opacity, Row::Blur]);
                }
                y = self.section(x, y, width, "Appearance", &rows, values);
                if let Some(edit) = &values.editor {
                    let title = format!("Theme Editor · {}", edit.palette.name);
                    y = self.section(x, y, width, &title, &Row::EDITOR, values);
                }
                // The sample first: the font list drops over the rows under Font.
                let text_rows = [Row::Sample, Row::Font, Row::Zoom, Row::LineSpacing, Row::Ligatures];
                y = self.section(x, y, width, "Text", &text_rows, values);
                self.section(x, y, width, "Cursor", &[Row::CursorShape, Row::Blink], values)
            }
            Section::Terminal => {
                let mut shell = vec![Row::Shell, Row::Fetch];
                if values.fetch > 0 {
                    shell.push(Row::FetchConfig);
                }
                y = self.section(x, y, width, "Shell", &shell, values);
                self.section(
                    x,
                    y,
                    width,
                    "Tabs",
                    &[Row::TabsTop, Row::Extended, Row::Notifications, Row::ResumePrograms],
                    values,
                )
            }
            Section::Ai => {
                let provider = PROVIDERS[values.provider];
                let mut ai = vec![Row::Provider];
                if provider.is("custom") {
                    ai.push(Row::Field(Field::LlmUrl));
                }
                if provider.takes_key() {
                    ai.push(Row::Field(Field::LlmKey));
                }
                if !provider.is("off") {
                    ai.push(Row::Field(Field::LlmModel));
                }
                ai.push(Row::Field(Field::TypesafeKey));
                self.section(x, y, width, "AI", &ai, values)
            }
            Section::Remote => {
                let mut rows = vec![Row::Remote];
                rows.extend((0..values.remote_phones.len()).map(Row::Connected));
                rows.push(Row::Phone);
                self.section(x, y, width, "Remote Control", &rows, values)
            }
            Section::Keys => self.keys(x, y, width, values),
            Section::Updates => {
                self.section(x, y, width, "Updates", &[Row::Updates, Row::Analytics, Row::CheckForUpdates], values)
            }
        }
    }
}
