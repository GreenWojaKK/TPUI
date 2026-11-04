// Package lex turns source text into a flat table of colored tokens.
//
// Scanning reads the text once from start to end and looks only at the text, so a file's tokens depend on nothing but
// its bytes. All the context a token needs is the token before it. A language is data: its words, its comment and
// string rules, and a hook for the few forms that do not fit them. Token kinds are hints for drawing and for deciding
// where to look; the text itself stays the source of truth.
package lex

import (
	"sort"
	"unicode"
	"unicode/utf8"
)

// Kind is what a token is. The values are part of the protocol with the viewer.
type Kind uint8

const (
	Comment Kind = iota + 1
	String
	Number
	Keyword
	Constant
	Type
	Function
	Attribute
)

func (k Kind) String() string {
	switch k {
	case Comment:
		return "comment"
	case String:
		return "string"
	case Number:
		return "number"
	case Keyword:
		return "keyword"
	case Constant:
		return "constant"
	case Type:
		return "type"
	case Function:
		return "function"
	case Attribute:
		return "attribute"
	}
	return "unknown"
}

// Table holds tokens as parallel arrays, sorted by position, without overlaps. Text outside every token is plain.
type Table struct {
	Starts []uint32
	Lens   []uint32
	Kinds  []Kind
}

// Len returns the number of tokens.
func (t *Table) Len() int { return len(t.Starts) }

// Range returns the indexes [from, to) of the tokens touching bytes [start, end).
func (t *Table) Range(start, end uint32) (from, to int) {
	from = sort.Search(t.Len(), func(i int) bool {
		return uint64(t.Starts[i])+uint64(t.Lens[i]) > uint64(start)
	})
	to = sort.Search(t.Len(), func(i int) bool { return t.Starts[i] >= end })
	return from, max(from, to)
}

// stringRule says how a string ends and what is special inside it.
type stringRule struct {
	closing string
	// escapes lets a backslash protect the next character.
	escapes bool
	// multiline lets the string run across lines. Otherwise a line break ends an unclosed string, which keeps the
	// damage of a missing quote to one line.
	multiline bool
	// fields makes `{ … }` code in which other strings do not end this one, and `{{` a plain brace.
	fields bool
}

// Language is the rules of one programming language.
type Language struct {
	Name string
	// lineComments run to the end of the line.
	lineComments []string
	// blockComments enables /* */, and nestedComments lets them nest.
	blockComments, nestedComments bool
	// strings maps an opening quote to its rule.
	strings map[byte]stringRule
	// words gives the kind of reserved words and well-known names.
	words map[string]Kind
	// definers gives the kind of the name right after a word, as with `func name`.
	definers map[string]Kind
	// dollarNames lets `$` appear in names.
	dollarNames bool
	// capitalCallIsType colors `Name(` as a type, in languages where such calls mostly construct.
	capitalCallIsType bool
	// macroBang colors `name!` as a function.
	macroBang bool
	// dotIsMember means a `.` before a name or at the start of a number always accesses a member: `1.max` and `x?.0`.
	dotIsMember bool
	// scan reads a token that the rules above do not cover at the current position, and returns whether it did.
	scan func(*scanner) bool
	// prefixed reads a string or similar form that starts with the word just read, and returns whether it did.
	prefixed func(s *scanner, start int, word string) bool
}

// previous is the token before the current position. Comments and white space do not replace it.
type previous struct {
	start, end int
	// endsValue is true after a name, a literal or a closing bracket, where a `/` divides.
	endsValue bool
	// lineBreak is true when a line break follows it. It starts true, for the first token of the text.
	lineBreak bool
}

type scanner struct {
	src   []byte
	pos   int
	table *Table
	lang  *Language
	prev  previous
	// templates holds, for each open `${` of a template string, the brace depth outside it.
	templates []int
	depth     int
}

// Lex reads a whole source text.
func Lex(lang *Language, src []byte) *Table {
	table := &Table{}
	if capacity := len(src) / 16; capacity > 0 {
		table.Starts = make([]uint32, 0, capacity)
		table.Lens = make([]uint32, 0, capacity)
		table.Kinds = make([]Kind, 0, capacity)
	}
	s := &scanner{src: src, table: table, lang: lang, prev: previous{lineBreak: true}}
	for s.pos < len(src) {
		s.step()
	}
	return table
}

func (s *scanner) step() {
	c := s.src[s.pos]
	switch c {
	case ' ', '\t', '\f', '\v':
		s.pos++
		return
	case '\n', '\r':
		s.pos++
		s.prev.lineBreak = true
		return
	}
	if s.comment() || (s.lang.scan != nil && s.lang.scan(s)) {
		return
	}
	if rule, ok := s.lang.strings[c]; ok {
		s.readString(s.pos, 1, rule)
		return
	}
	if isDigit(c) || (c == '.' && isDigit(s.peek(1)) && !s.lang.dotIsMember && !s.followsName()) {
		s.number()
		return
	}
	if r, size := s.runeAt(s.pos); s.isNameStart(r) {
		s.name(size)
	} else {
		s.punctuation(c, size)
	}
}

func (s *scanner) peek(offset int) byte {
	if s.pos+offset < len(s.src) {
		return s.src[s.pos+offset]
	}
	return 0
}

func (s *scanner) hasPrefix(prefix string) bool {
	return s.pos+len(prefix) <= len(s.src) && string(s.src[s.pos:s.pos+len(prefix)]) == prefix
}

func (s *scanner) prevText() []byte { return s.src[s.prev.start:s.prev.end] }

// followsName tells whether the current position continues a value or the word right before it, as the `.` of `x.0`
// and `self.0` does, rather than starting something new.
func (s *scanner) followsName() bool {
	return s.prev.endsValue || (s.prev.end == s.pos && s.prev.end > s.prev.start && isNameByte(s.src[s.prev.end-1]))
}

// emit records a token from `start` to the current position and makes it the previous token.
func (s *scanner) emit(start int, kind Kind) {
	s.add(start, kind)
	s.remember(start, kind != Keyword && kind != Attribute)
}

func (s *scanner) add(start int, kind Kind) {
	if s.pos > start {
		s.table.Starts = append(s.table.Starts, uint32(start))
		s.table.Lens = append(s.table.Lens, uint32(s.pos-start))
		s.table.Kinds = append(s.table.Kinds, kind)
	}
}

func (s *scanner) remember(start int, endsValue bool) {
	s.prev = previous{start: start, end: s.pos, endsValue: endsValue}
}

// punctuation reads an operator or bracket. A run of `.`, `+` or `-` is one token, so `..` and `...` are not member
// accesses and `++` can end a value.
func (s *scanner) punctuation(c byte, size int) {
	start := s.pos
	s.pos += size
	if c == '.' || c == '+' || c == '-' {
		for s.pos < len(s.src) && s.src[s.pos] == c {
			s.pos++
		}
	}
	endsValue := false
	switch c {
	case ')', ']':
		endsValue = true
	case '{':
		s.depth++
	case '}':
		s.depth = max(0, s.depth-1)
		endsValue = true
	case '+', '-':
		endsValue = s.pos-start >= 2
	}
	s.remember(start, endsValue)
}

func (s *scanner) comment() bool {
	for _, prefix := range s.lang.lineComments {
		if s.hasPrefix(prefix) {
			start := s.pos
			s.skipToLineEnd()
			s.add(start, Comment)
			return true
		}
	}
	if !s.lang.blockComments || !s.hasPrefix("/*") {
		return false
	}
	start := s.pos
	s.pos += 2
	for depth := 1; s.pos < len(s.src) && depth > 0; {
		switch {
		case s.hasPrefix("*/"):
			depth--
			s.pos += 2
		case s.lang.nestedComments && s.hasPrefix("/*"):
			depth++
			s.pos += 2
		default:
			s.pos++
		}
	}
	s.add(start, Comment)
	return true
}

func (s *scanner) skipToLineEnd() {
	for s.pos < len(s.src) && !isLineBreak(s.src[s.pos]) {
		s.pos++
	}
}

// readString reads a string that started at `start` and whose opening takes `open` bytes from the current position.
func (s *scanner) readString(start, open int, rule stringRule) {
	s.pos += open
	s.stringBody(rule)
	s.emit(start, String)
}

func (s *scanner) stringBody(rule stringRule) {
	fields := 0
	for s.pos < len(s.src) {
		c := s.src[s.pos]
		switch {
		case isLineBreak(c) && !rule.multiline:
			return
		case c == '\\' && rule.escapes && fields == 0:
			s.skipEscape()
		case fields > 0:
			switch c {
			case '{':
				fields++
				s.pos++
			case '}':
				fields--
				s.pos++
			case '"', '\'':
				// A string inside a field, which follows the outer string's line rule.
				s.pos++
				s.stringBody(stringRule{closing: string(c), escapes: true, multiline: rule.multiline})
			default:
				s.pos++
			}
		case s.hasPrefix(rule.closing):
			s.pos += len(rule.closing)
			return
		case c == '{' && rule.fields:
			if s.peek(1) == '{' {
				s.pos += 2
			} else {
				fields = 1
				s.pos++
			}
		default:
			s.pos++
		}
	}
}

// skipEscape moves past a backslash and the character after it, with CRLF counted as one.
func (s *scanner) skipEscape() {
	s.pos++
	if s.hasPrefix("\r\n") {
		s.pos += 2
	} else if s.pos < len(s.src) {
		_, size := s.runeAt(s.pos)
		s.pos += size
	}
}

// number reads a numeric literal with its suffix, such as 0xFF, 1_000, 1.5e-3, 1u8, 2i, 3j or 4n.
func (s *scanner) number() {
	start := s.pos
	if s.src[s.pos] == '0' && (s.peek(1)|0x20 == 'x' || s.peek(1)|0x20 == 'o' || s.peek(1)|0x20 == 'b') {
		hex := s.peek(1)|0x20 == 'x'
		s.pos += 2
		for s.pos < len(s.src) {
			c := s.src[s.pos]
			if hex && c|0x20 == 'p' && (s.peek(1) == '+' || s.peek(1) == '-') {
				s.pos += 2
			} else if isNameByte(c) || (hex && c == '.' && isDigit(s.peek(1))) {
				s.pos++
			} else {
				break
			}
		}
	} else {
		s.digits()
		if s.peek(0) == '.' && s.peek(1) != '.' {
			if r, _ := s.runeAt(s.pos + 1); !s.lang.dotIsMember || !s.isNameStart(r) {
				s.pos++
				s.digits()
			}
		}
		if s.peek(0)|0x20 == 'e' {
			sign := 0
			if s.peek(1) == '+' || s.peek(1) == '-' {
				sign = 1
			}
			if isDigit(s.peek(1 + sign)) {
				s.pos += 1 + sign
				s.digits()
			}
		}
		for s.pos < len(s.src) && isNameByte(s.src[s.pos]) {
			s.pos++
		}
	}
	s.emit(start, Number)
}

func (s *scanner) digits() {
	for s.pos < len(s.src) && (isDigit(s.src[s.pos]) || s.src[s.pos] == '_') {
		s.pos++
	}
}

func (s *scanner) name(firstSize int) {
	start := s.pos
	s.pos += firstSize
	s.nameRest()
	word := string(s.src[start:s.pos])
	if s.lang.prefixed != nil && s.lang.prefixed(s, start, word) {
		return
	}
	if kind := s.nameKind(start, word); kind != 0 {
		s.emit(start, kind)
	} else {
		s.remember(start, true)
	}
}

func (s *scanner) nameRest() {
	for s.pos < len(s.src) {
		c := s.src[s.pos]
		if c < utf8.RuneSelf {
			if !isNameByte(c) && !(c == '$' && s.lang.dollarNames) {
				return
			}
			s.pos++
			continue
		}
		r, size := s.runeAt(s.pos)
		if !unicode.IsLetter(r) && !unicode.IsDigit(r) && !unicode.Is(unicode.Mn, r) && !unicode.Is(unicode.Mc, r) {
			return
		}
		s.pos += size
	}
}

// nameKind decides a name's kind from the token before it and the text after it. 0 is a plain name.
func (s *scanner) nameKind(start int, word string) Kind {
	if kind, ok := s.lang.definers[string(s.prevText())]; ok {
		return kind
	}
	// A dot right before the name makes it a member, whatever its spelling. `from . import x` has a space.
	member := s.prev.end == start && string(s.prevText()) == "."
	if !member {
		if kind, ok := s.lang.words[word]; ok {
			return kind
		}
	}
	capital := isCapitalized(word)
	switch {
	case s.nextIsCall():
		if capital && s.lang.capitalCallIsType {
			return Type
		}
		return Function
	case capital:
		return Type
	case !member && isAllCaps(word):
		return Constant
	}
	return 0
}

// nextIsCall tells whether the name just read is followed by `(`, or by `!` in languages with such macros.
func (s *scanner) nextIsCall() bool {
	pos := s.pos
	for pos < len(s.src) && (s.src[pos] == ' ' || s.src[pos] == '\t') {
		pos++
	}
	if pos >= len(s.src) {
		return false
	}
	if s.src[pos] == '(' {
		return true
	}
	return s.lang.macroBang && pos == s.pos && s.src[pos] == '!' && (pos+1 >= len(s.src) || s.src[pos+1] != '=')
}

// attribute reads a decorator or attribute whose opening takes `open` bytes, up to the end of its dotted name.
func (s *scanner) attribute(open int) {
	start := s.pos
	s.pos += open
	for s.pos < len(s.src) && (isNameByte(s.src[s.pos]) || s.src[s.pos] == '.' || s.src[s.pos] == ':') {
		s.pos++
	}
	s.emit(start, Attribute)
}

func (s *scanner) runeAt(pos int) (rune, int) {
	if pos >= len(s.src) {
		return utf8.RuneError, 1
	}
	if c := s.src[pos]; c < utf8.RuneSelf {
		return rune(c), 1
	}
	return utf8.DecodeRune(s.src[pos:])
}

func (s *scanner) isNameStart(r rune) bool {
	if r < utf8.RuneSelf {
		return r == '_' || (r|0x20 >= 'a' && r|0x20 <= 'z') || (r == '$' && s.lang.dollarNames)
	}
	return r != utf8.RuneError && unicode.IsLetter(r)
}

func isDigit(c byte) bool     { return c >= '0' && c <= '9' }
func isLineBreak(c byte) bool { return c == '\n' || c == '\r' }

func isNameByte(c byte) bool {
	return c == '_' || isDigit(c) || (c|0x20 >= 'a' && c|0x20 <= 'z')
}

// isCapitalized is true for names like `Foo` and `FooBar`: an upper-case start with a lower-case letter later.
func isCapitalized(word string) bool {
	if len(word) < 2 || word[0] < 'A' || word[0] > 'Z' {
		return false
	}
	for i := 1; i < len(word); i++ {
		if word[i] >= 'a' && word[i] <= 'z' {
			return true
		}
	}
	return false
}

// isAllCaps is true for names like `MAX_SIZE`.
func isAllCaps(word string) bool {
	if len(word) < 2 {
		return false
	}
	letter := false
	for i := 0; i < len(word); i++ {
		c := word[i]
		switch {
		case c >= 'A' && c <= 'Z':
			letter = true
		case c != '_' && !isDigit(c):
			return false
		}
	}
	return letter
}
