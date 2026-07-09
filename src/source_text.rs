use rune::ast::{Span, StrSource};

pub fn slice<'a>(source: &'a str, span: Span) -> &'a str {
	&source[span.start.into_usize()..span.end.into_usize()]
}

/// Line number (1-based) on which the given span starts.
pub fn line_of(source: &str, span: Span) -> usize {
	source[..span.start.into_usize()]
		.bytes()
		.filter(|b| *b == b'\n')
		.count()
		+ 1
}

/// Returns the text content of a string literal as written in the source
/// code (without the surrounding quotes, with common escape sequences expanded).
pub fn lit_str_value(source: &str, span: Span, str_source: &StrSource) -> String {
	let StrSource::Text(text) = str_source else {
		// A synthetic literal (created in a macro) — does not occur in parsed
		// user scripts, but let's not panic on it.
		return String::new();
	};

	let span = if text.wrapped {
		Span::new(span.start.into_usize() + 1, span.end.into_usize() - 1)
	} else {
		span
	};

	let raw = slice(source, span);

	if !text.escaped {
		return raw.to_string();
	}

	unescape(raw)
}

fn unescape(input: &str) -> String {
	let mut out = String::with_capacity(input.len());
	let mut chars = input.chars();

	while let Some(c) = chars.next() {
		if c != '\\' {
			out.push(c);
			continue;
		}

		match chars.next() {
			Some('n') => out.push('\n'),
			Some('t') => out.push('\t'),
			Some('r') => out.push('\r'),
			Some('0') => out.push('\0'),
			Some('\\') => out.push('\\'),
			Some('"') => out.push('"'),
			Some('\'') => out.push('\''),
			Some(other) => out.push(other),
			None => {}
		}
	}

	out
}
