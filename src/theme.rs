use ratatui::style::Color;
use std::sync::{Mutex, OnceLock};

#[derive(Clone)]
pub struct Theme {
    pub name: &'static str,
    pub bg_primary: Color,
    pub bg_secondary: Color,
    pub bg_panel: Color,
    pub bg_input_active: Color,
    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_muted: Color,
    pub green: Color,
    pub yellow: Color,
    pub red: Color,
    pub blue: Color,
    pub cyan: Color,
    pub magenta: Color,
    pub orange: Color,
    pub border_focused: Color,
    pub border_normal: Color,
}

impl Theme {
    pub fn bar_colors(&self) -> [Color; 7] {
        [self.blue, self.green, self.cyan, self.magenta, self.orange, self.yellow, self.red]
    }

    pub fn utilization_color(&self, pct: f64) -> Color {
        if pct < 0.5 { self.green }
        else if pct < 0.8 { self.yellow }
        else { self.red }
    }
}

// ── All built-in themes ──

pub fn all_themes() -> &'static [Theme] {
    &THEMES
}

static THEMES: [Theme; 20] = [
    // 0: Midnight Blue (original)
    Theme {
        name: "Midnight Blue",
        bg_primary: Color::Rgb(26, 26, 46),
        bg_secondary: Color::Rgb(22, 33, 62),
        bg_panel: Color::Rgb(18, 22, 40),
        bg_input_active: Color::Rgb(20, 30, 55),
        text_primary: Color::Rgb(230, 230, 230),
        text_secondary: Color::Rgb(153, 153, 153),
        text_muted: Color::Rgb(102, 102, 102),
        green: Color::Rgb(76, 175, 80),
        yellow: Color::Rgb(255, 193, 7),
        red: Color::Rgb(244, 67, 54),
        blue: Color::Rgb(66, 133, 244),
        cyan: Color::Rgb(0, 188, 212),
        magenta: Color::Rgb(171, 71, 188),
        orange: Color::Rgb(255, 152, 0),
        border_focused: Color::Rgb(66, 133, 244),
        border_normal: Color::Rgb(51, 51, 51),
    },
    // 1: Dracula
    Theme {
        name: "Dracula",
        bg_primary: Color::Rgb(40, 42, 54),
        bg_secondary: Color::Rgb(68, 71, 90),
        bg_panel: Color::Rgb(33, 34, 44),
        bg_input_active: Color::Rgb(55, 57, 72),
        text_primary: Color::Rgb(248, 248, 242),
        text_secondary: Color::Rgb(189, 147, 249),
        text_muted: Color::Rgb(98, 114, 164),
        green: Color::Rgb(80, 250, 123),
        yellow: Color::Rgb(241, 250, 140),
        red: Color::Rgb(255, 85, 85),
        blue: Color::Rgb(139, 233, 253),
        cyan: Color::Rgb(139, 233, 253),
        magenta: Color::Rgb(255, 121, 198),
        orange: Color::Rgb(255, 184, 108),
        border_focused: Color::Rgb(189, 147, 249),
        border_normal: Color::Rgb(68, 71, 90),
    },
    // 2: Monokai Pro
    Theme {
        name: "Monokai Pro",
        bg_primary: Color::Rgb(45, 42, 46),
        bg_secondary: Color::Rgb(55, 52, 56),
        bg_panel: Color::Rgb(35, 33, 36),
        bg_input_active: Color::Rgb(50, 47, 51),
        text_primary: Color::Rgb(252, 252, 250),
        text_secondary: Color::Rgb(147, 146, 147),
        text_muted: Color::Rgb(90, 89, 90),
        green: Color::Rgb(169, 220, 118),
        yellow: Color::Rgb(255, 216, 102),
        red: Color::Rgb(255, 97, 136),
        blue: Color::Rgb(120, 220, 232),
        cyan: Color::Rgb(120, 220, 232),
        magenta: Color::Rgb(171, 157, 242),
        orange: Color::Rgb(252, 152, 103),
        border_focused: Color::Rgb(255, 216, 102),
        border_normal: Color::Rgb(73, 72, 73),
    },
    // 3: Solarized Dark
    Theme {
        name: "Solarized Dark",
        bg_primary: Color::Rgb(0, 43, 54),
        bg_secondary: Color::Rgb(7, 54, 66),
        bg_panel: Color::Rgb(0, 36, 46),
        bg_input_active: Color::Rgb(7, 54, 66),
        text_primary: Color::Rgb(253, 246, 227),
        text_secondary: Color::Rgb(147, 161, 161),
        text_muted: Color::Rgb(88, 110, 117),
        green: Color::Rgb(133, 153, 0),
        yellow: Color::Rgb(181, 137, 0),
        red: Color::Rgb(220, 50, 47),
        blue: Color::Rgb(38, 139, 210),
        cyan: Color::Rgb(42, 161, 152),
        magenta: Color::Rgb(211, 54, 130),
        orange: Color::Rgb(203, 75, 22),
        border_focused: Color::Rgb(38, 139, 210),
        border_normal: Color::Rgb(7, 54, 66),
    },
    // 4: Solarized Light
    Theme {
        name: "Solarized Light",
        bg_primary: Color::Rgb(253, 246, 227),
        bg_secondary: Color::Rgb(238, 232, 213),
        bg_panel: Color::Rgb(245, 239, 220),
        bg_input_active: Color::Rgb(238, 232, 213),
        text_primary: Color::Rgb(0, 43, 54),
        text_secondary: Color::Rgb(88, 110, 117),
        text_muted: Color::Rgb(147, 161, 161),
        green: Color::Rgb(133, 153, 0),
        yellow: Color::Rgb(181, 137, 0),
        red: Color::Rgb(220, 50, 47),
        blue: Color::Rgb(38, 139, 210),
        cyan: Color::Rgb(42, 161, 152),
        magenta: Color::Rgb(211, 54, 130),
        orange: Color::Rgb(203, 75, 22),
        border_focused: Color::Rgb(38, 139, 210),
        border_normal: Color::Rgb(147, 161, 161),
    },
    // 5: Gruvbox Dark
    Theme {
        name: "Gruvbox Dark",
        bg_primary: Color::Rgb(40, 40, 40),
        bg_secondary: Color::Rgb(60, 56, 54),
        bg_panel: Color::Rgb(29, 32, 33),
        bg_input_active: Color::Rgb(50, 48, 47),
        text_primary: Color::Rgb(235, 219, 178),
        text_secondary: Color::Rgb(189, 174, 147),
        text_muted: Color::Rgb(124, 111, 100),
        green: Color::Rgb(184, 187, 38),
        yellow: Color::Rgb(250, 189, 47),
        red: Color::Rgb(251, 73, 52),
        blue: Color::Rgb(131, 165, 152),
        cyan: Color::Rgb(142, 192, 124),
        magenta: Color::Rgb(211, 134, 155),
        orange: Color::Rgb(254, 128, 25),
        border_focused: Color::Rgb(250, 189, 47),
        border_normal: Color::Rgb(80, 73, 69),
    },
    // 6: Gruvbox Light
    Theme {
        name: "Gruvbox Light",
        bg_primary: Color::Rgb(251, 241, 199),
        bg_secondary: Color::Rgb(235, 219, 178),
        bg_panel: Color::Rgb(242, 229, 188),
        bg_input_active: Color::Rgb(235, 219, 178),
        text_primary: Color::Rgb(40, 40, 40),
        text_secondary: Color::Rgb(102, 92, 84),
        text_muted: Color::Rgb(146, 131, 116),
        green: Color::Rgb(121, 116, 14),
        yellow: Color::Rgb(181, 118, 20),
        red: Color::Rgb(204, 36, 29),
        blue: Color::Rgb(69, 133, 136),
        cyan: Color::Rgb(104, 157, 106),
        magenta: Color::Rgb(177, 98, 134),
        orange: Color::Rgb(214, 93, 14),
        border_focused: Color::Rgb(181, 118, 20),
        border_normal: Color::Rgb(168, 153, 132),
    },
    // 7: Nord
    Theme {
        name: "Nord",
        bg_primary: Color::Rgb(46, 52, 64),
        bg_secondary: Color::Rgb(59, 66, 82),
        bg_panel: Color::Rgb(40, 45, 56),
        bg_input_active: Color::Rgb(59, 66, 82),
        text_primary: Color::Rgb(236, 239, 244),
        text_secondary: Color::Rgb(216, 222, 233),
        text_muted: Color::Rgb(76, 86, 106),
        green: Color::Rgb(163, 190, 140),
        yellow: Color::Rgb(235, 203, 139),
        red: Color::Rgb(191, 97, 106),
        blue: Color::Rgb(129, 161, 193),
        cyan: Color::Rgb(136, 192, 208),
        magenta: Color::Rgb(180, 142, 173),
        orange: Color::Rgb(208, 135, 112),
        border_focused: Color::Rgb(136, 192, 208),
        border_normal: Color::Rgb(59, 66, 82),
    },
    // 8: Catppuccin Mocha
    Theme {
        name: "Catppuccin Mocha",
        bg_primary: Color::Rgb(30, 30, 46),
        bg_secondary: Color::Rgb(49, 50, 68),
        bg_panel: Color::Rgb(24, 24, 37),
        bg_input_active: Color::Rgb(45, 45, 60),
        text_primary: Color::Rgb(205, 214, 244),
        text_secondary: Color::Rgb(166, 173, 200),
        text_muted: Color::Rgb(108, 112, 134),
        green: Color::Rgb(166, 227, 161),
        yellow: Color::Rgb(249, 226, 175),
        red: Color::Rgb(243, 139, 168),
        blue: Color::Rgb(137, 180, 250),
        cyan: Color::Rgb(137, 220, 235),
        magenta: Color::Rgb(203, 166, 247),
        orange: Color::Rgb(250, 179, 135),
        border_focused: Color::Rgb(203, 166, 247),
        border_normal: Color::Rgb(69, 71, 90),
    },
    // 9: Catppuccin Latte
    Theme {
        name: "Catppuccin Latte",
        bg_primary: Color::Rgb(239, 241, 245),
        bg_secondary: Color::Rgb(220, 224, 232),
        bg_panel: Color::Rgb(230, 233, 239),
        bg_input_active: Color::Rgb(220, 224, 232),
        text_primary: Color::Rgb(76, 79, 105),
        text_secondary: Color::Rgb(108, 111, 133),
        text_muted: Color::Rgb(156, 160, 176),
        green: Color::Rgb(64, 160, 43),
        yellow: Color::Rgb(223, 142, 29),
        red: Color::Rgb(210, 15, 57),
        blue: Color::Rgb(30, 102, 245),
        cyan: Color::Rgb(4, 165, 229),
        magenta: Color::Rgb(136, 57, 239),
        orange: Color::Rgb(254, 100, 11),
        border_focused: Color::Rgb(136, 57, 239),
        border_normal: Color::Rgb(172, 176, 190),
    },
    // 10: Tokyo Night
    Theme {
        name: "Tokyo Night",
        bg_primary: Color::Rgb(26, 27, 38),
        bg_secondary: Color::Rgb(36, 40, 59),
        bg_panel: Color::Rgb(22, 22, 30),
        bg_input_active: Color::Rgb(41, 46, 66),
        text_primary: Color::Rgb(192, 202, 245),
        text_secondary: Color::Rgb(130, 137, 175),
        text_muted: Color::Rgb(86, 95, 137),
        green: Color::Rgb(158, 206, 106),
        yellow: Color::Rgb(224, 175, 104),
        red: Color::Rgb(247, 118, 142),
        blue: Color::Rgb(122, 162, 247),
        cyan: Color::Rgb(125, 207, 255),
        magenta: Color::Rgb(187, 154, 247),
        orange: Color::Rgb(255, 158, 100),
        border_focused: Color::Rgb(122, 162, 247),
        border_normal: Color::Rgb(41, 46, 66),
    },
    // 11: One Dark
    Theme {
        name: "One Dark",
        bg_primary: Color::Rgb(40, 44, 52),
        bg_secondary: Color::Rgb(50, 55, 65),
        bg_panel: Color::Rgb(33, 37, 43),
        bg_input_active: Color::Rgb(55, 60, 72),
        text_primary: Color::Rgb(171, 178, 191),
        text_secondary: Color::Rgb(130, 137, 151),
        text_muted: Color::Rgb(92, 99, 112),
        green: Color::Rgb(152, 195, 121),
        yellow: Color::Rgb(229, 192, 123),
        red: Color::Rgb(224, 108, 117),
        blue: Color::Rgb(97, 175, 239),
        cyan: Color::Rgb(86, 182, 194),
        magenta: Color::Rgb(198, 120, 221),
        orange: Color::Rgb(209, 154, 102),
        border_focused: Color::Rgb(97, 175, 239),
        border_normal: Color::Rgb(62, 68, 81),
    },
    // 12: Cyberpunk
    Theme {
        name: "Cyberpunk",
        bg_primary: Color::Rgb(13, 2, 33),
        bg_secondary: Color::Rgb(25, 10, 55),
        bg_panel: Color::Rgb(8, 0, 22),
        bg_input_active: Color::Rgb(30, 15, 65),
        text_primary: Color::Rgb(0, 255, 255),
        text_secondary: Color::Rgb(255, 0, 255),
        text_muted: Color::Rgb(100, 60, 140),
        green: Color::Rgb(0, 255, 65),
        yellow: Color::Rgb(255, 255, 0),
        red: Color::Rgb(255, 0, 68),
        blue: Color::Rgb(0, 145, 255),
        cyan: Color::Rgb(0, 255, 255),
        magenta: Color::Rgb(255, 0, 255),
        orange: Color::Rgb(255, 150, 0),
        border_focused: Color::Rgb(255, 0, 255),
        border_normal: Color::Rgb(50, 20, 90),
    },
    // 13: Synthwave '84
    Theme {
        name: "Synthwave '84",
        bg_primary: Color::Rgb(38, 20, 71),
        bg_secondary: Color::Rgb(52, 28, 95),
        bg_panel: Color::Rgb(30, 15, 58),
        bg_input_active: Color::Rgb(60, 35, 105),
        text_primary: Color::Rgb(255, 230, 230),
        text_secondary: Color::Rgb(230, 150, 210),
        text_muted: Color::Rgb(130, 80, 140),
        green: Color::Rgb(114, 242, 114),
        yellow: Color::Rgb(254, 228, 64),
        red: Color::Rgb(254, 68, 80),
        blue: Color::Rgb(54, 206, 255),
        cyan: Color::Rgb(54, 206, 255),
        magenta: Color::Rgb(255, 121, 198),
        orange: Color::Rgb(255, 167, 89),
        border_focused: Color::Rgb(255, 121, 198),
        border_normal: Color::Rgb(65, 40, 100),
    },
    // 14: Matrix
    Theme {
        name: "Matrix",
        bg_primary: Color::Rgb(0, 10, 0),
        bg_secondary: Color::Rgb(0, 20, 5),
        bg_panel: Color::Rgb(0, 5, 0),
        bg_input_active: Color::Rgb(0, 25, 8),
        text_primary: Color::Rgb(0, 255, 65),
        text_secondary: Color::Rgb(0, 180, 50),
        text_muted: Color::Rgb(0, 100, 30),
        green: Color::Rgb(0, 255, 65),
        yellow: Color::Rgb(120, 255, 0),
        red: Color::Rgb(255, 50, 50),
        blue: Color::Rgb(0, 200, 120),
        cyan: Color::Rgb(0, 255, 180),
        magenta: Color::Rgb(0, 220, 100),
        orange: Color::Rgb(180, 255, 0),
        border_focused: Color::Rgb(0, 255, 65),
        border_normal: Color::Rgb(0, 60, 20),
    },
    // 15: Rose Pine
    Theme {
        name: "Rose Pine",
        bg_primary: Color::Rgb(25, 23, 36),
        bg_secondary: Color::Rgb(38, 35, 53),
        bg_panel: Color::Rgb(21, 19, 30),
        bg_input_active: Color::Rgb(42, 39, 58),
        text_primary: Color::Rgb(224, 222, 244),
        text_secondary: Color::Rgb(144, 140, 170),
        text_muted: Color::Rgb(110, 106, 134),
        green: Color::Rgb(156, 207, 216),
        yellow: Color::Rgb(246, 193, 119),
        red: Color::Rgb(235, 111, 146),
        blue: Color::Rgb(49, 116, 143),
        cyan: Color::Rgb(156, 207, 216),
        magenta: Color::Rgb(196, 167, 231),
        orange: Color::Rgb(234, 154, 151),
        border_focused: Color::Rgb(196, 167, 231),
        border_normal: Color::Rgb(38, 35, 53),
    },
    // 16: Everforest Dark
    Theme {
        name: "Everforest Dark",
        bg_primary: Color::Rgb(47, 53, 47),
        bg_secondary: Color::Rgb(58, 65, 57),
        bg_panel: Color::Rgb(39, 44, 39),
        bg_input_active: Color::Rgb(55, 62, 55),
        text_primary: Color::Rgb(211, 198, 170),
        text_secondary: Color::Rgb(157, 149, 130),
        text_muted: Color::Rgb(113, 108, 95),
        green: Color::Rgb(167, 192, 128),
        yellow: Color::Rgb(219, 188, 127),
        red: Color::Rgb(230, 126, 128),
        blue: Color::Rgb(127, 187, 179),
        cyan: Color::Rgb(131, 192, 179),
        magenta: Color::Rgb(214, 153, 182),
        orange: Color::Rgb(230, 152, 117),
        border_focused: Color::Rgb(167, 192, 128),
        border_normal: Color::Rgb(68, 76, 66),
    },
    // 17: Kanagawa
    Theme {
        name: "Kanagawa",
        bg_primary: Color::Rgb(31, 31, 40),
        bg_secondary: Color::Rgb(43, 43, 55),
        bg_panel: Color::Rgb(22, 22, 28),
        bg_input_active: Color::Rgb(48, 48, 62),
        text_primary: Color::Rgb(220, 215, 186),
        text_secondary: Color::Rgb(146, 139, 115),
        text_muted: Color::Rgb(84, 84, 109),
        green: Color::Rgb(152, 187, 108),
        yellow: Color::Rgb(226, 194, 103),
        red: Color::Rgb(195, 64, 67),
        blue: Color::Rgb(126, 156, 216),
        cyan: Color::Rgb(106, 149, 137),
        magenta: Color::Rgb(149, 127, 184),
        orange: Color::Rgb(255, 160, 102),
        border_focused: Color::Rgb(126, 156, 216),
        border_normal: Color::Rgb(54, 54, 70),
    },
    // 18: Ayu Dark
    Theme {
        name: "Ayu Dark",
        bg_primary: Color::Rgb(10, 14, 20),
        bg_secondary: Color::Rgb(20, 25, 35),
        bg_panel: Color::Rgb(5, 8, 13),
        bg_input_active: Color::Rgb(25, 30, 42),
        text_primary: Color::Rgb(191, 191, 191),
        text_secondary: Color::Rgb(107, 122, 140),
        text_muted: Color::Rgb(68, 82, 100),
        green: Color::Rgb(170, 217, 76),
        yellow: Color::Rgb(255, 180, 84),
        red: Color::Rgb(255, 51, 51),
        blue: Color::Rgb(54, 163, 217),
        cyan: Color::Rgb(149, 230, 203),
        magenta: Color::Rgb(217, 118, 195),
        orange: Color::Rgb(255, 143, 64),
        border_focused: Color::Rgb(54, 163, 217),
        border_normal: Color::Rgb(30, 40, 55),
    },
    // 19: Palenight
    Theme {
        name: "Palenight",
        bg_primary: Color::Rgb(41, 45, 62),
        bg_secondary: Color::Rgb(50, 55, 77),
        bg_panel: Color::Rgb(34, 38, 53),
        bg_input_active: Color::Rgb(55, 60, 82),
        text_primary: Color::Rgb(166, 172, 205),
        text_secondary: Color::Rgb(130, 137, 175),
        text_muted: Color::Rgb(96, 102, 138),
        green: Color::Rgb(195, 232, 141),
        yellow: Color::Rgb(255, 203, 107),
        red: Color::Rgb(255, 83, 112),
        blue: Color::Rgb(130, 170, 255),
        cyan: Color::Rgb(137, 221, 255),
        magenta: Color::Rgb(199, 146, 234),
        orange: Color::Rgb(247, 140, 108),
        border_focused: Color::Rgb(199, 146, 234),
        border_normal: Color::Rgb(55, 60, 82),
    },
];

// ── Global active theme ──

static ACTIVE_THEME: OnceLock<Mutex<usize>> = OnceLock::new();

fn theme_index() -> &'static Mutex<usize> {
    ACTIVE_THEME.get_or_init(|| Mutex::new(0))
}

pub fn active() -> Theme {
    let idx = *theme_index().lock().unwrap();
    THEMES[idx].clone()
}

pub fn active_index() -> usize {
    *theme_index().lock().unwrap()
}

pub fn set_active(index: usize) {
    if index < THEMES.len() {
        *theme_index().lock().unwrap() = index;
    }
}

// ── Convenience accessors (match the old constant-style API) ──

macro_rules! theme_color {
    ($name:ident, $field:ident) => {
        #[inline]
        pub fn $name() -> Color { active().$field }
    };
}

theme_color!(bg_primary, bg_primary);
theme_color!(bg_secondary, bg_secondary);
theme_color!(bg_panel, bg_panel);
theme_color!(bg_input_active, bg_input_active);
theme_color!(text_primary, text_primary);
theme_color!(text_secondary, text_secondary);
theme_color!(text_muted, text_muted);

// Named color accessors used as theme::GREEN() etc.
// Keep UPPER_CASE names so call-sites read like the old constants.
#[allow(non_snake_case)] pub fn GREEN() -> Color { active().green }
#[allow(non_snake_case)] pub fn YELLOW() -> Color { active().yellow }
#[allow(non_snake_case)] pub fn RED() -> Color { active().red }
#[allow(non_snake_case)] pub fn BLUE() -> Color { active().blue }
#[allow(non_snake_case)] pub fn CYAN() -> Color { active().cyan }
#[allow(non_snake_case)] pub fn MAGENTA() -> Color { active().magenta }
#[allow(non_snake_case)] pub fn ORANGE() -> Color { active().orange }
#[allow(non_snake_case)] pub fn BORDER_FOCUSED() -> Color { active().border_focused }
#[allow(non_snake_case)] pub fn BORDER_NORMAL() -> Color { active().border_normal }
#[allow(non_snake_case)] pub fn BAR_COLORS() -> [Color; 7] { active().bar_colors() }
pub fn utilization_color(pct: f64) -> Color { active().utilization_color(pct) }

// ── Persistence ──

fn config_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("claude-commander").join("config.json"))
}

/// Load the saved theme index from disk and apply it.
pub fn load_saved() {
    let Some(path) = config_path() else { return };
    let Ok(data) = std::fs::read_to_string(&path) else { return };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) else { return };
    if let Some(idx) = val.get("theme_index").and_then(|v| v.as_u64()) {
        set_active(idx as usize);
    }
}

/// Save the current theme index to disk.
pub fn save_current() {
    let Some(path) = config_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let val = serde_json::json!({ "theme_index": active_index() });
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&val).unwrap_or_default());
}
