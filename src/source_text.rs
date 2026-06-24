use rune::ast::{Span, StrSource};

pub fn slice<'a>(source: &'a str, span: Span) -> &'a str {
	&source[span.start.into_usize()..span.end.into_usize()]
}

/// Vrátí textový obsah řetězcového literálu tak, jak je zapsaný ve zdrojovém
/// kódu (bez okolních uvozovek, s rozbalenými běžnými escape sekvencemi).
pub fn lit_str_value(source: &str, span: Span, str_source: &StrSource) -> String {
	let StrSource::Text(text) = str_source else {
		// Syntetický literál (vznikl v makru) — v parsovaném uživatelském
		// skriptu nenastává, ale ať to nepanikaří.
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
