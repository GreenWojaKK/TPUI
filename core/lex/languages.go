package lex

import (
	"path/filepath"
	"strings"
)

// Detect returns the language of a file by its extension, or nil when no rules exist for it.
func Detect(path string) *Language {
	switch strings.ToLower(filepath.Ext(path)) {
	case ".py", ".pyi":
		return Python
	case ".go":
		return Go
	case ".rs":
		return Rust
	case ".ts", ".tsx", ".mts", ".cts":
		return TypeScript
	case ".js", ".jsx", ".mjs", ".cjs":
		return JavaScript
	}
	return nil
}

// wordsOf builds a word table from lists of words per kind.
func wordsOf(lists map[Kind]string) map[string]Kind {
	table := map[string]Kind{}
	for kind, list := range lists {
		for _, word := range strings.Fields(list) {
			table[word] = kind
		}
	}
	return table
}

// ----------------------------------------------------------------------- Python

// Python is the rules of Python.
var Python = &Language{
	Name:         "Python",
	lineComments: []string{"#"},
	words: wordsOf(map[Kind]string{
		Constant: "False None True",
		Keyword: `and as assert async await break class continue def del elif else except finally for from global if
			import in is lambda nonlocal not or pass raise return try while with yield`,
	}),
	definers:          map[string]Kind{"def": Function, "class": Type},
	capitalCallIsType: true,
	scan: func(s *scanner) bool {
		switch c := s.src[s.pos]; {
		case c == '"' || c == '\'':
			s.pythonString(s.pos, false)
		case c == '@' && s.prev.lineBreak:
			s.attribute(1)
		default:
			return false
		}
		return true
	},
	prefixed: func(s *scanner, start int, word string) bool {
		if c := s.peek(0); c != '"' && c != '\'' {
			return false
		}
		switch strings.ToLower(word) {
		case "r", "b", "u", "rb", "br":
			s.pythonString(start, false)
		case "f", "t", "fr", "rf", "tr", "rt":
			s.pythonString(start, true)
		default:
			return false
		}
		return true
	},
}

// pythonString reads a string from its opening quote at the current position. Triple quotes run across lines.
func (s *scanner) pythonString(start int, fields bool) {
	quote := string(s.src[s.pos])
	if s.hasPrefix(quote + quote + quote) {
		s.readString(start, 3, stringRule{closing: quote + quote + quote, escapes: true, multiline: true, fields: fields})
	} else {
		s.readString(start, 1, stringRule{closing: quote, escapes: true, fields: fields})
	}
}

// ----------------------------------------------------------------------- Go

// Go is the rules of Go.
var Go = &Language{
	Name:          "Go",
	lineComments:  []string{"//"},
	blockComments: true,
	strings: map[byte]stringRule{
		'"':  {closing: `"`, escapes: true},
		'\'': {closing: `'`, escapes: true},
		'`':  {closing: "`", multiline: true},
	},
	words: wordsOf(map[Kind]string{
		Type: `bool byte complex64 complex128 error float32 float64 int int8 int16 int32 int64 rune string uint uint8
			uint16 uint32 uint64 uintptr any comparable`,
		Constant: "true false nil iota",
		Keyword: `break case chan const continue default defer else fallthrough for func go goto if import interface
			map package range return select struct switch type var`,
	}),
	definers:        map[string]Kind{"func": Function, "type": Type},
	caseMarksExport: true,
}

// ----------------------------------------------------------------------- Rust

// Rust is the rules of Rust.
var Rust = &Language{
	Name:           "Rust",
	lineComments:   []string{"//"},
	blockComments:  true,
	nestedComments: true,
	strings:        map[byte]stringRule{'"': {closing: `"`, escapes: true, multiline: true}},
	words: wordsOf(map[Kind]string{
		Type:     "i8 i16 i32 i64 i128 isize u8 u16 u32 u64 u128 usize f32 f64 bool char str",
		Constant: "true false",
		Keyword: `as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod
			move mut pub ref return self Self static struct super trait type unsafe use where while yield`,
	}),
	definers:          map[string]Kind{"fn": Function, "struct": Type, "enum": Type, "trait": Type, "type": Type},
	capitalCallIsType: true,
	macroBang:         true,
	dotIsMember:       true,
	scan: func(s *scanner) bool {
		switch s.src[s.pos] {
		case '\'':
			return s.rustQuote()
		case '#':
			if s.peek(1) == '[' {
				s.attribute(2)
				return true
			}
			if s.peek(1) == '!' && s.peek(2) == '[' {
				s.attribute(3)
				return true
			}
		}
		return false
	},
	prefixed: func(s *scanner, start int, word string) bool {
		switch word {
		case "b", "c":
			if s.peek(0) == '"' {
				s.readString(start, 1, stringRule{closing: `"`, escapes: true, multiline: true})
				return true
			}
			if word == "b" && s.peek(0) == '\'' {
				s.readString(start, 1, stringRule{closing: `'`, escapes: true})
				return true
			}
		case "r", "br", "cr":
			return s.rustRawString(start, word)
		}
		return false
	},
}

// rustRawString reads the rest of r"…" or r#"…"#, or of a raw name r#name, after the word.
func (s *scanner) rustRawString(start int, word string) bool {
	hashes := 0
	for s.peek(hashes) == '#' {
		hashes++
	}
	if s.peek(hashes) == '"' {
		s.readString(start, hashes+1, stringRule{closing: `"` + strings.Repeat("#", hashes), multiline: true})
		return true
	}
	if r, size := s.runeAt(s.pos + 1); word == "r" && hashes == 1 && s.isNameStart(r) {
		s.pos += 1 + size
		s.nameRest()
		s.remember(start, true)
		return true
	}
	return false
}

// rustQuote tells a character from a lifetime: a lifetime is a name without a closing quote right after its first
// character.
func (s *scanner) rustQuote() bool {
	start := s.pos
	if s.peek(1) == '\\' {
		s.readString(start, 1, stringRule{closing: `'`, escapes: true})
		return true
	}
	r, size := s.runeAt(s.pos + 1)
	if s.peek(1+size) == '\'' {
		s.pos += 2 + size
		s.emit(start, String)
		return true
	}
	if !s.isNameStart(r) {
		return false
	}
	s.pos += 1 + size
	s.nameRest()
	s.emit(start, Type)
	return true
}

// ----------------------------------------------------------------------- JavaScript and TypeScript

const (
	scriptKeywords = `break case catch class const continue debugger default delete do else export extends finally for
		from function get if import in instanceof let new of return set static switch throw try typeof var void while
		with yield async await`
	scriptConstants = "true false null undefined this super NaN Infinity"
)

var scriptStrings = map[byte]stringRule{
	'"':  {closing: `"`, escapes: true},
	'\'': {closing: `'`, escapes: true},
}

// JavaScript is the rules of JavaScript, JSX included.
var JavaScript = &Language{
	Name:              "JavaScript",
	lineComments:      []string{"//"},
	blockComments:     true,
	strings:           scriptStrings,
	dollarNames:       true,
	capitalCallIsType: true,
	words:             wordsOf(map[Kind]string{Constant: scriptConstants, Keyword: scriptKeywords}),
	definers:          map[string]Kind{"function": Function, "class": Type},
	scan:              scanScript,
}

// TypeScript is the rules of TypeScript, TSX included.
var TypeScript = &Language{
	Name:              "TypeScript",
	lineComments:      []string{"//"},
	blockComments:     true,
	strings:           scriptStrings,
	dollarNames:       true,
	capitalCallIsType: true,
	words: wordsOf(map[Kind]string{
		Type:     "any bigint boolean never number object string symbol unknown",
		Constant: scriptConstants,
		Keyword: scriptKeywords + ` abstract as asserts declare enum implements interface is keyof module namespace
			override private protected public readonly require satisfies type`,
	}),
	definers: map[string]Kind{"function": Function, "class": Type, "interface": Type, "type": Type, "enum": Type},
	scan:     scanScript,
}

func scanScript(s *scanner) bool {
	switch s.src[s.pos] {
	case '`':
		s.templateText(s.pos)
		return true
	case '}':
		// The end of a template's `${ … }` goes back to its text.
		if n := len(s.templates); n > 0 && s.depth == s.templates[n-1] {
			s.templates = s.templates[:n-1]
			s.templateText(s.pos)
			return true
		}
	case '/':
		return !s.prev.endsValue && s.regularExpression()
	case '@':
		if r, _ := s.runeAt(s.pos + 1); s.isNameStart(r) {
			s.attribute(1)
			return true
		}
	case '#':
		if s.pos == 0 && s.peek(1) == '!' {
			s.skipToLineEnd()
			s.add(0, Comment)
			return true
		}
	}
	return false
}

// templateText reads a template string's text from its opening backtick or `}`, up to the closing backtick or the next
// `${`, which is included. What follows a `${` is ordinary code until the matching `}`.
func (s *scanner) templateText(start int) {
	s.pos++
	for s.pos < len(s.src) {
		switch c := s.src[s.pos]; {
		case c == '\\':
			s.skipEscape()
		case c == '`':
			s.pos++
			s.emit(start, String)
			return
		case c == '$' && s.peek(1) == '{':
			s.pos += 2
			s.add(start, String)
			s.remember(start, false)
			s.templates = append(s.templates, s.depth)
			return
		default:
			s.pos++
		}
	}
	s.emit(start, String)
}

// maxRegularExpression bounds the look-ahead for a closing `/`, so that a line of many slashes stays linear.
const maxRegularExpression = 1 << 16

// regularExpression reads a regular expression literal. It returns false, reading nothing, when the line has no closing
// `/`, since the slash then divides.
func (s *scanner) regularExpression() bool {
	start := s.pos
	limit := min(len(s.src), start+maxRegularExpression)
	inClass := false
	for pos := start + 1; pos < limit; pos++ {
		switch c := s.src[pos]; {
		case isLineBreak(c):
			return false
		case c == '\\':
			pos++
		case inClass:
			inClass = c != ']'
		case c == '[':
			inClass = true
		case c == '/':
			s.pos = pos + 1
			for s.pos < len(s.src) && isNameByte(s.src[s.pos]) {
				s.pos++
			}
			s.emit(start, String)
			return true
		}
	}
	return false
}
