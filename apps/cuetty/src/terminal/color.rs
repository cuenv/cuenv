//! Colour arithmetic for terminal text, kept pure so it is tested without a window.

use super::model::Rgb;

/// How far dim text moves towards its background. Halving a colour's channels only
/// reads as quieter on a dark background: on a light one it makes text bolder.
const DIM_TOWARDS_BACKGROUND: f32 = 0.5;

/// `amount` of the way from one colour to another.
pub(super) fn blend(from: Rgb, to: Rgb, amount: f32) -> Rgb {
    let amount = amount.clamp(0.0, 1.0);
    let channel = |from: u8, to: u8| {
        (f32::from(from) + (f32::from(to) - f32::from(from)) * amount).round() as u8
    };
    Rgb(
        channel(from.0, to.0),
        channel(from.1, to.1),
        channel(from.2, to.2),
    )
}

/// Dim text, faded towards what it is drawn on.
pub(super) fn dimmed(foreground: Rgb, background: Rgb) -> Rgb {
    blend(foreground, background, DIM_TOWARDS_BACKGROUND)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: Rgb = Rgb(30, 30, 46);
    const LIGHT: Rgb = Rgb(239, 241, 245);

    #[test]
    fn dim_text_moves_towards_its_background_on_either_theme() {
        assert!(dimmed(Rgb(205, 214, 244), DARK).0 < 205);
        // Halving the channels would have darkened this and added contrast.
        assert!(dimmed(Rgb(76, 79, 105), LIGHT).0 > 76);
    }

    #[test]
    fn blending_reaches_both_ends_and_ignores_overshoot() {
        assert_eq!(blend(DARK, LIGHT, 0.0), DARK);
        assert_eq!(blend(DARK, LIGHT, 1.0), LIGHT);
        assert_eq!(blend(DARK, LIGHT, 7.0), LIGHT);
    }
}
