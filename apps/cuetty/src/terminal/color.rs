//! Colour arithmetic for terminal text, kept pure so it is tested without a window.

use super::config::MinimumContrast;
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

/// WCAG 2.1 relative luminance.
fn relative_luminance(Rgb(red, green, blue): Rgb) -> f32 {
    let channel = |value: u8| {
        let value = f32::from(value) / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue)
}

/// WCAG 2.1 contrast ratio, from 1 for identical colours to 21 for black on white.
pub(super) fn contrast_ratio(first: Rgb, second: Rgb) -> f32 {
    let (first, second) = (relative_luminance(first), relative_luminance(second));
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

/// Whether dark ink reads better on `color` than light ink does.
pub(super) fn is_light(color: Rgb) -> bool {
    contrast_ratio(color, BLACK) > contrast_ratio(color, WHITE)
}

/// How finely [`with_minimum_contrast`] searches for the smallest change, enough to
/// land within a colour step.
const CONTRAST_SEARCH_STEPS: u32 = 8;

const WHITE: Rgb = Rgb(255, 255, 255);
const BLACK: Rgb = Rgb(0, 0, 0);

/// The colour nearest `foreground` that reaches the minimum against `background`,
/// moving towards whichever of black or white offers more contrast. Where neither gets
/// there, as on a mid grey with a high minimum, this is the best that one can do.
pub(super) fn with_minimum_contrast(
    foreground: Rgb,
    background: Rgb,
    minimum: MinimumContrast,
) -> Rgb {
    let minimum = minimum.ratio();
    if contrast_ratio(foreground, background) >= minimum {
        return foreground;
    }
    let extreme = if contrast_ratio(WHITE, background) >= contrast_ratio(BLACK, background) {
        WHITE
    } else {
        BLACK
    };
    if contrast_ratio(extreme, background) < minimum {
        return extreme;
    }
    let (_, amount) = (0..CONTRAST_SEARCH_STEPS).fold((0.0_f32, 1.0_f32), |(low, high), _| {
        let middle = (low + high) / 2.0;
        if contrast_ratio(blend(foreground, extreme, middle), background) >= minimum {
            (low, middle)
        } else {
            (middle, high)
        }
    });
    blend(foreground, extreme, amount)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: Rgb = Rgb(30, 30, 46);
    const LIGHT: Rgb = Rgb(239, 241, 245);

    fn minimum(ratio: f32) -> MinimumContrast {
        MinimumContrast::new(ratio).expect("a valid ratio")
    }

    #[test]
    fn contrast_runs_from_identical_to_black_on_white() {
        assert!((contrast_ratio(DARK, DARK) - 1.0).abs() < 0.001);
        assert!((contrast_ratio(BLACK, WHITE) - 21.0).abs() < 0.001);
        assert_eq!(contrast_ratio(BLACK, WHITE), contrast_ratio(WHITE, BLACK));
    }

    #[test]
    fn dim_text_moves_towards_its_background_on_either_theme() {
        let on_dark = dimmed(Rgb(205, 214, 244), DARK);
        assert!(contrast_ratio(on_dark, DARK) < contrast_ratio(Rgb(205, 214, 244), DARK));
        let on_light = dimmed(Rgb(76, 79, 105), LIGHT);
        // Halving the channels would have darkened this and added contrast.
        assert!(contrast_ratio(on_light, LIGHT) < contrast_ratio(Rgb(76, 79, 105), LIGHT));
        assert!(on_light.0 > 76);
    }

    #[test]
    fn blending_reaches_both_ends_and_ignores_overshoot() {
        assert_eq!(blend(DARK, LIGHT, 0.0), DARK);
        assert_eq!(blend(DARK, LIGHT, 1.0), LIGHT);
        assert_eq!(blend(DARK, LIGHT, 7.0), LIGHT);
    }

    #[test]
    fn colours_already_meeting_the_minimum_are_left_alone() {
        let text = Rgb(205, 214, 244);
        assert_eq!(with_minimum_contrast(text, DARK, minimum(4.5)), text);
    }

    #[test]
    fn off_leaves_even_invisible_text_alone() {
        assert_eq!(
            with_minimum_contrast(DARK, DARK, MinimumContrast::OFF),
            DARK
        );
    }

    #[test]
    fn dark_text_on_dark_lightens_just_far_enough() {
        let adjusted = with_minimum_contrast(Rgb(50, 50, 60), DARK, minimum(4.5));
        let ratio = contrast_ratio(adjusted, DARK);
        assert!(ratio >= 4.5, "reached only {ratio}");
        assert!(ratio < 4.8, "went further than it needed to: {ratio}");
    }

    #[test]
    fn light_text_on_light_darkens() {
        let adjusted = with_minimum_contrast(Rgb(220, 220, 225), LIGHT, minimum(4.5));
        assert!(contrast_ratio(adjusted, LIGHT) >= 4.5);
        assert!(adjusted.0 < 220);
    }

    #[test]
    fn a_minimum_no_colour_can_reach_gives_the_best_available() {
        let grey = Rgb(119, 119, 119);
        let adjusted = with_minimum_contrast(Rgb(120, 120, 120), grey, minimum(21.0));
        assert_eq!(adjusted, BLACK);
    }
}
