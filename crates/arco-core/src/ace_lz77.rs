use crate::files;
use std::{
    io::{self, Read, Write},
    sync::atomic::AtomicBool,
};

pub(crate) const COMPRESSION: [u8; 6] = [1, 1, 1, 0, 0, 0];
pub(crate) const BLOCKED: [u8; 6] = [2, 1, 1, 0, 0, 0];
const BLOCK: usize = 16 * 1024;
const DICTIONARY: usize = 2048;
const MAX_MATCH: usize = 255;
const MODE_SYMBOL: usize = 283;
const MAIN_COUNT: usize = MODE_SYMBOL + 1;

#[derive(Clone, Copy, Default)]
struct Code {
    width: u8,
    value: u32,
}

// ACE assigns equal-width codes in the order produced by its pivot partition.
fn order(symbols: &mut [usize], widths: &[u8], mut left: usize, mut right: usize) {
    let start = left;
    let end = right;
    let pivot = widths[symbols[right]];
    loop {
        while widths[symbols[left]] > pivot {
            left += 1;
        }
        while widths[symbols[right]] < pivot {
            right -= 1;
        }
        if left <= right {
            symbols.swap(left, right);
            left += 1;
            if right == 0 {
                break;
            }
            right -= 1;
        }
        if left >= right {
            break;
        }
    }
    for (first, last) in [(start, right), (left, end)] {
        if first + 1 < last {
            order(symbols, widths, first, last);
        } else if first < last && widths[symbols[first]] < widths[symbols[last]] {
            symbols.swap(first, last);
        }
    }
}

fn codes<const N: usize>(widths: &[u8; N]) -> [Code; N] {
    let mut symbols = std::array::from_fn::<_, N, _>(|index| index);
    order(&mut symbols, widths, 0, N - 1);
    let mut output = [Code::default(); N];
    let mut position = 0;
    for symbol in symbols.into_iter().rev() {
        let width = widths[symbol];
        if width != 0 {
            output[symbol] = Code {
                width,
                value: position >> (11 - width),
            };
            position += 1 << (11 - width);
        }
    }
    output
}

struct BitWriter<W> {
    output: W,
    word: u32,
    filled: u8,
    bytes: u64,
}

impl<W: Write> BitWriter<W> {
    fn write(&mut self, value: u32, width: u8) -> io::Result<()> {
        let available = 32 - self.filled;
        if width < available {
            self.word = (self.word << width) | value;
            self.filled += width;
            return Ok(());
        }
        let extra = width - available;
        self.word = (self.word << available) | (value >> extra);
        self.output.write_all(&self.word.to_le_bytes())?;
        self.bytes += 4;
        self.word = value & ((1 << extra) - 1);
        self.filled = extra;
        Ok(())
    }

    fn code(&mut self, code: Code) -> io::Result<()> {
        self.write(code.value, code.width)
    }

    fn tree(&mut self, widths: &[u8], lengths: &[Code; 4]) -> io::Result<()> {
        // Actual widths 8 and 9 are transmitted as modulo-three deltas.
        self.write((widths.len() - 1) as u32, 9)?;
        self.write(7, 4)?;
        self.write(3, 4)?;
        for width in [1, 1, 0, 0] {
            self.write(width, 3)?;
        }
        let mut previous = 0;
        for &width in widths {
            let value = width - 7;
            self.code(lengths[((value + 3 - previous) % 3) as usize])?;
            previous = value;
        }
        Ok(())
    }

    fn finish(mut self) -> io::Result<u64> {
        if self.filled != 0 {
            self.output
                .write_all(&(self.word << (32 - self.filled)).to_le_bytes())?;
            self.bytes += 4;
        }
        // A decoder peeks up to eleven bits before consuming the last code.
        self.output.write_all(&[0; 4])?;
        Ok(self.bytes + 4)
    }
}

enum Symbol {
    Literal(u8),
    Match { distance: usize, length: usize },
}

fn match_fields(distance: usize, length: usize) -> (usize, u32, u8, usize) {
    let value = (distance - 1) as u32;
    let width = (32 - value.leading_zeros()) as u8;
    let extra = width.saturating_sub(1);
    let bits = value & ((1 << extra) - 1);
    let length = length - if value <= 255 { 2 } else { 3 };
    (260 + usize::from(width), bits, extra, length)
}

fn bit_cost(tokens: &[Symbol], widths: &[u8; MAIN_COUNT]) -> usize {
    tokens
        .iter()
        .map(|token| match *token {
            Symbol::Literal(byte) => usize::from(widths[usize::from(byte)]),
            Symbol::Match { distance, length } => {
                let (main, _, extra, _) = match_fields(distance, length);
                usize::from(widths[main] + extra + 8)
            }
        })
        .sum()
}

fn symbols(block: &[u8], output: &mut Vec<Symbol>) {
    output.clear();
    let mut recent = [usize::MAX; DICTIONARY];
    let hash = |at: usize| {
        let prefix = u32::from_le_bytes([block[at], block[at + 1], block[at + 2], 0]);
        (prefix.wrapping_mul(0x9e37_79b1) >> 21) as usize
    };
    let mut position = 0;
    while position + 2 < block.len() {
        let candidate = recent[hash(position)];
        let distance = position.wrapping_sub(candidate);
        let mut length = 0;
        if candidate != usize::MAX && distance <= DICTIONARY {
            let limit = MAX_MATCH.min(block.len() - position);
            while length < limit && block[candidate + length] == block[position + length] {
                length += 1;
            }
        }
        let consumed = if length >= 4 {
            output.push(Symbol::Match { distance, length });
            length
        } else {
            output.push(Symbol::Literal(block[position]));
            1
        };
        for at in position..(position + consumed).min(block.len() - 2) {
            recent[hash(at)] = at;
        }
        position += consumed;
    }
    output.extend(block[position..].iter().copied().map(Symbol::Literal));
}

pub(crate) fn encode(
    mut input: impl Read,
    output: impl Write,
    blocked: bool,
    cancel: &AtomicBool,
) -> io::Result<(u64, u64, u32)> {
    // 228 eight-bit codes and 56 nine-bit codes fill one prefix tree.
    let main_widths =
        std::array::from_fn::<_, MAIN_COUNT, _>(|index| if index < 228 { 8 } else { 9 });
    let match_widths = [8; 256];
    let width_codes = codes(&[1, 1, 0, 0]);
    let main_codes = codes(&main_widths);
    let match_codes = codes(&match_widths);
    let mut writer = BitWriter {
        output,
        word: 0,
        filled: 0,
        bytes: 0,
    };
    let mut crc = crc32fast::Hasher::new();
    let mut block = Vec::with_capacity(BLOCK);
    let mut tokens = Vec::with_capacity(BLOCK);
    let mut delta_block = Vec::new();
    let mut delta_tokens = Vec::new();
    let mut last_delta = 0u8;
    let mut delta_mode = false;
    let mut input_size = 0;
    loop {
        files::check_cancel(cancel)?;
        block.clear();
        let size = input.by_ref().take(BLOCK as u64).read_to_end(&mut block)?;
        if size == 0 {
            break;
        }
        input_size += size as u64;
        crc.update(&block);
        symbols(&block, &mut tokens);
        let mut use_delta = false;
        if blocked {
            delta_block.clear();
            let mut previous = last_delta;
            delta_block.extend(block.iter().map(|&byte| {
                let delta = byte.wrapping_sub(previous);
                previous = byte;
                delta
            }));
            symbols(&delta_block, &mut delta_tokens);
            use_delta =
                bit_cost(&delta_tokens, &main_widths) + 42 < bit_cost(&tokens, &main_widths);
            if use_delta {
                last_delta = previous;
            }
        }
        let mode_change = use_delta || delta_mode;
        let tokens = if use_delta { &delta_tokens } else { &tokens };
        writer.tree(&main_widths, &width_codes)?;
        writer.tree(&match_widths, &width_codes)?;
        writer.write(tokens.len() as u32 + u32::from(mode_change), 15)?;
        if mode_change {
            writer.code(main_codes[MODE_SYMBOL])?;
            writer.write(u32::from(use_delta), 8)?;
            if use_delta {
                writer.write(1, 8)?; // One byte plane.
                writer.write(size as u32, 17)?;
            }
        }
        delta_mode = use_delta;
        for token in tokens {
            match *token {
                Symbol::Literal(byte) => writer.code(main_codes[usize::from(byte)])?,
                Symbol::Match { distance, length } => {
                    let (main, bits, extra, length) = match_fields(distance, length);
                    writer.code(main_codes[main])?;
                    writer.write(bits, extra)?;
                    writer.code(match_codes[length])?;
                }
            }
        }
    }
    Ok((input_size, writer.finish()?, !crc.finalize()))
}
