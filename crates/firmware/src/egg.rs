//! The Easter egg behind a ten-second hold: the face flies past the stars
//! while a rainbow runs along the lights.
//!
//! One frame number draws the screen and colours the lights, so they stay in
//! step.

use embedded_graphics::{
    image::{Image, ImageDrawableExt, ImageRaw},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
};

pub const FRAME_MS: u64 = 100;

/// The face with its eyes open, then blinking, above six sparkle frames.
/// `#` is a lit pixel.
const ART: [&str; HEIGHT] = [
    "..##############################....##############################..",
    ".################################..################################.",
    "####################################################################",
    "####################################################################",
    "########..##############..##########################################",
    "########..######..######..########################..################",
    "########..######..######..################..######..######..########",
    "########..######..######..################..######..######..########",
    "########..######..######..################..######..######..########",
    "################..################################..################",
    "################..################################..################",
    "################..################################..################",
    "################..################################..################",
    "################..################################..################",
    "################..################################..################",
    "################..################################..################",
    "##############....##############################....################",
    "##############....##############################....################",
    "####################################################################",
    "########....##########....################....##########....########",
    "########....##########....################....##########....########",
    "###########............######################............###########",
    "###########............######################............###########",
    "####################################################################",
    ".################################..################################.",
    "..##############################....##############################..",
    "........................#......#......#.............................",
    ".................#......#....#...#..................................",
    "..........#......#..................................................",
    "...#.....###...##.##.##.#.###.....##.....#..........................",
    "..........#......#..................................................",
    ".................#......#....#...#..................................",
    "........................#......#......#.............................",
];
const WIDTH: usize = 68;
const HEIGHT: usize = 33;
const STRIDE: usize = WIDTH.div_ceil(8);
const PIXELS: [u8; STRIDE * HEIGHT] = pack(ART);
const SHEET: ImageRaw<BinaryColor> = ImageRaw::new(&PIXELS, WIDTH as u32);
const FACE: Size = Size::new(34, 26);
const SPARKLE: Size = Size::new(7, 7);
const FACE_AT: Point = Point::new(47, 19);

/// Trail columns that move together.
const BLOCK: i32 = 8;
const STRIPE: u32 = 3;
const STRIPE_PITCH: i32 = 5;

/// The distance a star travels before it comes round again.
const FIELD: u64 = 240;
/// Each star's starting column, row, pixels a frame and sparkle phase.
const STARS: [(u64, i32, u64, u64); 12] = [
    (0, 1, 6, 0),
    (70, 8, 4, 3),
    (118, 3, 8, 5),
    (160, 11, 6, 1),
    (210, 5, 4, 4),
    (20, 28, 6, 2),
    (140, 35, 8, 4),
    (55, 48, 4, 5),
    (5, 55, 8, 2),
    (95, 51, 6, 0),
    (150, 56, 4, 3),
    (200, 49, 6, 1),
];

pub fn draw<D: DrawTarget<Color = BinaryColor>>(
    target: &mut D,
    frame: u64,
) -> Result<(), D::Error> {
    let sprite = |target: &mut D, cell: Point, size: Size, at: Point| {
        Image::new(&SHEET.sub_image(&Rectangle::new(cell, size)), at).draw(target)
    };
    target.clear(BinaryColor::Off)?;
    for (start, row, speed, phase) in STARS {
        let x = (FIELD - 16) as i32 - ((start + speed * frame) % FIELD) as i32;
        let cell = Point::new(((frame + phase) % 6) as i32 * 7, FACE.height as i32);
        sprite(target, cell, SPARKLE, Point::new(x, row))?;
    }

    // The trail stays put and hides the stars behind it. Its blocks count
    // back from the face; alternate ones sit a pixel apart and swap every
    // other frame.
    let end = FACE_AT.x - 1;
    fill(
        target,
        0,
        FACE_AT.y,
        end as u32,
        FACE.height,
        BinaryColor::Off,
    )?;
    for block in 0..=end / BLOCK {
        let left = end - (block + 1) * BLOCK;
        let top = FACE_AT.y + 1 + (block + (frame / 2 % 2) as i32) % 2;
        for stripe in 0..5 {
            let y = top + stripe * STRIPE_PITCH;
            fill(target, left, y, BLOCK as u32, STRIPE, BinaryColor::On)?;
        }
    }

    // The face only bobs. A dark pixel separates it from the trail and stars.
    let at = FACE_AT + Point::new(0, i32::from(matches!(frame % 6, 2..=4)));
    fill(
        target,
        at.x - 1,
        at.y - 1,
        FACE.width + 2,
        FACE.height + 2,
        BinaryColor::Off,
    )?;
    let blink = frame % 60 >= 58;
    let cell = Point::new(if blink { FACE.width as i32 } else { 0 }, 0);
    sprite(target, cell, FACE, at)
}

/// RGB values in chain order: the status light, then port 5 through port 1.
/// Each light is a sixth of the colour wheel from the next, and the colours
/// travel towards port 1 as the stars do.
pub fn lights(frame: u64) -> [[u8; 3]; 6] {
    core::array::from_fn(|light| {
        let hue = ((4 * frame + 16 * (5 - light as u64)) % 96) as u8;
        let (rise, fall) = (hue % 32, 32 - hue % 32);
        match hue / 32 {
            0 => [fall, rise, 0],
            1 => [0, fall, rise],
            _ => [rise, 0, fall],
        }
    })
}

/// One bit a pixel, most significant first, each row whole bytes.
const fn pack(art: [&str; HEIGHT]) -> [u8; STRIDE * HEIGHT] {
    let mut pixels = [0; STRIDE * HEIGHT];
    let mut y = 0;
    while y < HEIGHT {
        let row = art[y].as_bytes();
        assert!(row.len() == WIDTH);
        let mut x = 0;
        while x < WIDTH {
            if row[x] == b'#' {
                pixels[y * STRIDE + x / 8] |= 0x80 >> (x % 8);
            }
            x += 1;
        }
        y += 1;
    }
    pixels
}

fn fill<D: DrawTarget<Color = BinaryColor>>(
    target: &mut D,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    color: BinaryColor,
) -> Result<(), D::Error> {
    Rectangle::new(Point::new(x, y), Size::new(width, height))
        .into_styled(PrimitiveStyle::with_fill(color))
        .draw(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_graphics::image::GetPixel;

    struct Screen([[bool; 128]; 64]);

    impl OriginDimensions for Screen {
        fn size(&self) -> Size {
            Size::new(128, 64)
        }
    }

    impl DrawTarget for Screen {
        type Color = BinaryColor;
        type Error = core::convert::Infallible;

        fn draw_iter<I: IntoIterator<Item = Pixel<BinaryColor>>>(
            &mut self,
            pixels: I,
        ) -> Result<(), Self::Error> {
            for Pixel(point, color) in pixels {
                let row = self.0.get_mut(point.y as usize);
                if let Some(pixel) = row.and_then(|row| row.get_mut(point.x as usize)) {
                    *pixel = color.is_on();
                }
            }
            Ok(())
        }
    }

    #[test]
    fn the_face_stays_whole_in_the_middle_of_the_screen_and_only_bobs() {
        let sheet = SHEET;
        assert_eq!(sheet.size(), Size::new(2 * FACE.width, FACE.height + 7));
        assert_eq!(FACE_AT.x as u32 * 2 + FACE.width, 128);
        assert_eq!(FACE_AT.y as u32 * 2 + FACE.height, 64);
        for frame in 0..240 {
            let mut screen = Screen([[false; 128]; 64]);
            draw(&mut screen, frame).unwrap();
            let top = FACE_AT.y as usize + usize::from(matches!(frame % 6, 2..=4));
            let cell = if frame % 60 >= 58 { FACE.width } else { 0 };
            for point in Rectangle::new(Point::zero(), FACE).points() {
                let lit = sheet
                    .pixel(point + Point::new(cell as i32, 0))
                    .unwrap()
                    .is_on();
                let (x, y) = (point.x as usize + 47, point.y as usize + top);
                assert_eq!(screen.0[y][x], lit, "frame {frame} at {x},{y}");
            }
            // Nothing touches its sides.
            for row in &screen.0[top..top + 26] {
                assert!(!row[46] && !row[81]);
            }
        }
    }
}
