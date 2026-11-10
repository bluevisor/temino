use super::*;

/// Every row showing: Custom shows every AI field.
fn sample() -> Values {
    Values {
        background: Some(0),
        theme: "Temino".into(),
        themes: 12,
        palettes: Vec::new(),
        palette: 0,
        custom: false,
        editor: None,
        bindings: 0,
        ligatures: true,
        font: "JetBrains Mono".into(),
        quick_terminal: Some(None),
        opacity: 0,
        blur: false,
        shell: Some(0),
        installed: [true, true, false],
        fetch: 1,
        fetches_installed: [true, false, true],
        tabs_top: false,
        extended: false,
        notifications: true,
        zoom: 100.0,
        points: 15.4,
        line_spacing: 1,
        cursor: 0,
        blink: true,
        updates: true,
        analytics: true,
        see_through: true,
        remote: None,
        remote_account: String::new(),
        remote_url: String::new(),
        remote_phones: Vec::new(),
        provider: PROVIDERS.len() - 1,
        fields: Default::default(),
        editing: None,
    }
}

#[test]
fn every_control_can_be_clicked() {
    let mut scene = Scene::new();
    let mut text = TextSystem::new(14.0);
    let theme = Theme::dark();
    let values = sample();
    let mut page = Page::default();
    let rect = Rect::new(200.0, 0.0, 900.0, 2000.0);
    scene.reset(1100.0, 2000.0, 1.0, theme.background);
    page.draw(&mut scene, &mut text, &theme, rect, &values, (0.0, 0.0));
    let actions: Vec<Action> = page.hits.iter().map(|&(_, a)| a).collect();
    // A preset in use: Customize only offers a copy.
    let mut expected =
        vec![Action::Theme(-1), Action::ThemeList, Action::Theme(1), Action::DuplicateTheme, Action::OpenThemes];
    expected.extend((0..BACKGROUNDS.len()).map(Action::Background));
    expected.extend((0..OPACITIES.len()).map(Action::Opacity));
    expected.extend([
        Action::Blur(true),
        Action::Font(-1),
        Action::Font(1),
        Action::Font(1),
        Action::Zoom(-1),
        Action::Zoom(0),
        Action::Zoom(1),
    ]);
    expected.extend((0..LINE_SPACINGS.len()).map(Action::LineSpacing));
    expected.push(Action::Ligatures(false));
    expected.extend((0..Shape::ALL.len()).map(Action::Cursor));
    expected.push(Action::Blink(false));
    expected.extend((0..KINDS.len()).map(Action::Shell));
    expected.extend((0..=FETCHES.len()).map(Action::Fetch));
    expected.push(Action::FetchConfig);
    expected.extend([Action::EditKeys, Action::QuickTerminal(true)]);
    expected.extend([
        Action::TabsTop(false),
        Action::TabsTop(true),
        Action::Extended(true),
        Action::Notifications(false),
    ]);
    expected.extend((0..PROVIDERS.len()).map(Action::Provider));
    expected.extend(Field::ALL.map(Action::Edit));
    expected.extend([Action::Updates(false), Action::Analytics(false), Action::CheckForUpdates]);
    assert_eq!(actions, expected);
    // A click lands on what's under it, and nothing overlaps.
    for (rect, action) in &page.hits {
        let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        assert_eq!(page.action_at(x, y), Some(*action));
    }
}

#[test]
fn the_theme_editor_offers_every_color_of_a_theme_of_your_own() {
    let mut scene = Scene::new();
    let mut text = TextSystem::new(14.0);
    let theme = Theme::dark();
    let mut palette = crate::theme::library("/nonexistent")[1].copy_as("Mine");
    palette.file = Some("/nonexistent/Mine".into());
    let edit = ThemeEdit { palette, slot: Slot::Ansi(1), confirm_delete: false };
    let values = Values { custom: true, editor: Some(edit), ..sample() };
    let mut page = Page::default();
    let rect = Rect::new(200.0, 0.0, 900.0, 3000.0);
    scene.reset(1100.0, 3000.0, 1.0, theme.background);
    page.draw(&mut scene, &mut text, &theme, rect, &values, (0.0, 0.0));
    let actions: Vec<Action> = page.hits.iter().map(|&(_, a)| a).collect();
    // Customize: Done and Duplicate, right to left.
    let at = actions.iter().position(|&a| a == Action::Theme(1)).unwrap();
    assert_eq!(actions[at + 1..at + 4], [Action::ThemeEditor(false), Action::DuplicateTheme, Action::OpenThemes]);
    // The editor: name, 5 + 8 + 8 colors, hex, three nudges, delete.
    let start = actions.iter().position(|&a| a == Action::Edit(Field::ThemeName)).unwrap();
    let mut expected = vec![Action::Edit(Field::ThemeName)];
    expected.extend(Slot::BASE.map(Action::Slot));
    expected.extend((0..16).map(|i| Action::Slot(Slot::Ansi(i))));
    expected.push(Action::Edit(Field::ThemeHex));
    for channel in [Channel::Lightness, Channel::Saturation, Channel::Hue] {
        expected.extend((-1..=1).map(|step| Action::Nudge(channel, step)));
    }
    expected.push(Action::DeleteTheme);
    assert_eq!(actions[start..start + expected.len()], expected[..]);
    for (rect, action) in &page.hits {
        let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        assert_eq!(page.action_at(x, y), Some(*action), "{action:?}");
    }
    // A theme of your own with the editor closed offers to edit it.
    let values = Values { custom: true, ..sample() };
    page.draw(&mut scene, &mut text, &theme, rect, &values, (0.0, 0.0));
    let actions: Vec<Action> = page.hits.iter().map(|&(_, a)| a).collect();
    assert!(actions.contains(&Action::ThemeEditor(true)));
    assert!(!actions.iter().any(|a| matches!(a, Action::Slot(_) | Action::DeleteTheme)));
}

#[test]
fn the_page_scrolls_only_as_far_as_it_goes() {
    let mut page = Page { content_height: 900.0, view_height: 600.0, ..Page::default() };
    page.wheel(100.0, 0.0, 0.0);
    assert_eq!(page.scroll, 300.0);
    page.wheel(-100.0, 0.0, 0.0);
    assert_eq!(page.scroll, 0.0);
}

#[test]
fn drawing_clamps_a_scroll_the_content_no_longer_reaches() {
    let mut scene = Scene::new();
    let mut text = TextSystem::new(14.0);
    let theme = Theme::dark();
    let values = sample();
    // Scrolled to the end of 900px of content in a 600px view, then
    // the window grows past the whole content: nothing to scroll.
    let mut page = Page { content_height: 900.0, view_height: 600.0, ..Page::default() };
    page.scroll = 300.0;
    let rect = Rect::new(0.0, 0.0, 900.0, 2000.0);
    scene.reset(900.0, 2000.0, 1.0, theme.background);
    page.draw(&mut scene, &mut text, &theme, rect, &values, (0.0, 0.0));
    assert_eq!(page.scroll, 0.0);
    // Content still taller than the view: the scroll is capped at
    // the new end, not zeroed.
    page.content_height = 900.0;
    page.scroll = 500.0;
    let rect = Rect::new(0.0, 0.0, 900.0, 600.0);
    scene.reset(900.0, 600.0, 1.0, theme.background);
    page.draw(&mut scene, &mut text, &theme, rect, &values, (0.0, 0.0));
    assert_eq!(page.scroll, 300.0);
}

#[test]
fn keys_show_as_dots_and_nothing_else_does() {
    let secret: Vec<_> = Field::ALL.iter().filter(|f| f.secret()).copied().collect();
    assert_eq!(secret, [Field::LlmKey, Field::TypesafeKey]);
    // Each field reads its own slot in `Values::fields`.
    for (i, field) in Field::ALL.into_iter().enumerate() {
        assert_eq!(field.index(), Some(i));
    }
}

#[test]
fn a_page_that_fits_never_scrolls() {
    let mut page = Page { content_height: 400.0, view_height: 600.0, ..Page::default() };
    page.wheel(5.0, 0.0, 0.0);
    assert_eq!(page.scroll, 0.0);
    page.wheel(-5.0, 0.0, 0.0);
    assert_eq!(page.scroll, 0.0);
}

#[test]
fn the_wheel_scrolls_the_theme_list_only_while_over_it() {
    let list = Rect::new(100.0, 100.0, 200.0, 300.0);
    let mut page = Page { content_height: 2000.0, view_height: 600.0, list, ..Page::default() };
    page.toggle_themes();
    assert!(page.themes_open);
    page.list_scroll = 0.0;
    page.wheel(2.0, 150.0, 200.0);
    assert_eq!((page.list_scroll, page.scroll), (2.0 * LIST_ROW, 0.0));
    page.wheel(1.0, 50.0, 200.0);
    assert_eq!((page.list_scroll, page.scroll), (2.0 * LIST_ROW, WHEEL_STEP));
    // Closed, the list's old place scrolls the page.
    page.toggle_themes();
    assert!(!page.themes_open);
    page.wheel(1.0, 150.0, 200.0);
    assert_eq!(page.scroll, 2.0 * WHEEL_STEP);
}

#[test]
fn a_click_goes_to_what_was_drawn_last_under_it() {
    let row = Rect::new(0.0, 0.0, 400.0, 60.0);
    let list = Rect::new(100.0, 30.0, 200.0, 200.0);
    let page = Page { hits: vec![(row, Action::Ligatures(true)), (list, Action::PickTheme(3))], ..Page::default() };
    assert_eq!(page.action_at(10.0, 10.0), Some(Action::Ligatures(true)));
    assert_eq!(page.action_at(150.0, 40.0), Some(Action::PickTheme(3)));
    assert_eq!(page.action_at(150.0, 250.0), None);
    // Rectangles take their top-left edge, not their bottom-right one.
    assert_eq!(page.action_at(400.0, 10.0), None);
}
