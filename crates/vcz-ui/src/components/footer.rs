use ratatui::text::{Line, Span};

use crate::PALETTE;

static SPACING: &str = "\t\t";

pub fn footer() -> Line<'static> {
    let t = vec![
        Span::raw("[t]").style(PALETTE.purple),
        Span::raw("orrent"),
        Span::raw(SPACING),

        Span::raw("[n]").style(PALETTE.purple),
        Span::raw("etwork"),
        Span::raw(SPACING),

        Span::raw("[f]").style(PALETTE.purple),
        Span::raw("ilters"),
        Span::raw(SPACING),

        Span::raw("[d]").style(PALETTE.purple),
        Span::raw("elete"),
        Span::raw(SPACING),

        Span::raw("[h/j/k/l]").style(PALETTE.purple),
        Span::raw(" move"),
        Span::raw(SPACING),

        Span::raw("[Tab]").style(PALETTE.purple),
        Span::raw(" cycle"),
        Span::raw(SPACING),
    ];
    t.into()
}
