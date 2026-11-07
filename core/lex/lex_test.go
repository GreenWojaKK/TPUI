package lex

import (
	"bytes"
	"fmt"
	"strings"
	"testing"
)

// tokens lists a text's tokens as `kind:text`.
func tokens(lang *Language, src string) []string {
	table := Lex(lang, []byte(src))
	out := make([]string, table.Len())
	for i := range out {
		start := table.Starts[i]
		out[i] = fmt.Sprintf("%s:%s", table.Kinds[i], src[start:start+table.Lens[i]])
	}
	return out
}

func check(t *testing.T, lang *Language, src string, want ...string) {
	t.Helper()
	got := tokens(lang, src)
	if strings.Join(got, "\x00") != strings.Join(want, "\x00") {
		t.Errorf("%s:\n got %q\nwant %q", src, got, want)
	}
}

func TestPythonStrings(t *testing.T) {
	check(t, Python, `x = "a#b" # 주석`, `string:"a#b"`, `comment:# 주석`)
	check(t, Python, `'it\'s'`, `string:'it\'s'`)
	check(t, Python, "\"\"\"여러\n줄 ' \"\n\"\"\" + 1", "string:\"\"\"여러\n줄 ' \"\n\"\"\"", "number:1")
	check(t, Python, `rb'a' Rb"b" u'c' F"d"`, `string:rb'a'`, `string:Rb"b"`, `string:u'c'`, `string:F"d"`)
	check(t, Python, `rf = 1`, `number:1`)
	// A raw string still treats a backslash-quote as part of it.
	check(t, Python, `r'\'' + 1`, `string:r'\''`, `number:1`)
}

func TestPythonUnclosedStringsStopAtTheLineEnd(t *testing.T) {
	check(t, Python, "x = 'abc\ny = 1", `string:'abc`, `number:1`)
	check(t, Python, "'abc\\\ndef' + 1", "string:'abc\\\ndef'", "number:1")
	check(t, Python, "'abc\\\r\ndef'", "string:'abc\\\r\ndef'")
	// A triple-quoted string runs to the end of the file.
	check(t, Python, "'''abc\ny = 1", "string:'''abc\ny = 1")
}

func TestPythonFormattedStrings(t *testing.T) {
	check(t, Python, `f"{x["a"]} {{y}}" + 1`, `string:f"{x["a"]} {{y}}"`, `number:1`)
	check(t, Python, `f'{x!r:>{w}}'`, `string:f'{x!r:>{w}}'`)
	check(t, Python, `f"{'}'}" + 1`, `string:f"{'}'}"`, `number:1`)
	check(t, Python, "f'''{\n  a['b']\n}''' + 1", "string:f'''{\n  a['b']\n}'''", "number:1")
	check(t, Python, "f'{a\nb = 1", `string:f'{a`, `number:1`)
}

func TestPythonWords(t *testing.T) {
	check(t, Python, "def run(self):\n    return None",
		"keyword:def", "function:run", "keyword:return", "constant:None")
	check(t, Python, "class Foo(Bar):", "keyword:class", "type:Foo", "type:Bar")
	check(t, Python, "print(a.if_x, a.class_(), MAX_SIZE)", "function:print", "function:class_", "constant:MAX_SIZE")
	check(t, Python, "@app.route('/')\ndef f(): pass",
		"attribute:@app.route", `string:'/'`, "keyword:def", "function:f", "keyword:pass")
	check(t, Python, "a @ b")
	check(t, Python, "def f(): ...\ndef g(): ...", "keyword:def", "function:f", "keyword:def", "function:g")
	check(t, Python, "from . import x\nfrom .. import y", "keyword:from", "keyword:import", "keyword:from", "keyword:import")
	check(t, Python, "1.j 1.e5 1. 2.5j", "number:1.j", "number:1.e5", "number:1.", "number:2.5j")
	check(t, Python, "x.is_ok; x.is; x.import")
}

func TestGo(t *testing.T) {
	check(t, Go, "func (r T) Name(a int) string { return `raw\n\\` + \"x\\n\" + 'y' }",
		"keyword:func", "function:Name", "type:int", "type:string", "keyword:return",
		"string:`raw\n\\`", `string:"x\n"`, "string:'y'")
	check(t, Go, "type Foo struct{}", "keyword:type", "type:Foo", "keyword:struct")
	check(t, Go, "func main() {}", "keyword:func", "function:main")
	check(t, Go, "// 줄 주석\n/* 블록\n주석 */ x := nil", "comment:// 줄 주석", "comment:/* 블록\n주석 */", "constant:nil")
	check(t, Go, `Println("a")`, "function:Println", `string:"a"`)
	check(t, Go, "\"abc\nx := 1", `string:"abc`, "number:1")
	check(t, Go, "0x1p-2 1_000 1.5e3 3i 0b101 = .5", "number:0x1p-2", "number:1_000", "number:1.5e3", "number:3i", "number:0b101", "number:.5")
}

func TestGoCaseMarksExportsAndNotKinds(t *testing.T) {
	// Exported fields, variables and constants are plain names; only a defined type and the predeclared ones are types.
	check(t, Go, "type Endpoint struct {\n\tLabel string\n\tPoint Point\n\tID, URL int\n}",
		"keyword:type", "type:Endpoint", "keyword:struct", "type:string", "type:int")
	check(t, Go, "var MaxWalk = 1\nconst MAX_SIZE = 2", "keyword:var", "number:1", "keyword:const", "number:2")
	check(t, Go, "fmt.Println(r.Label, time.Duration(5))", "function:Println", "function:Duration", "number:5")
	check(t, Go, "func (r *Router) Analyze() {}", "keyword:func", "function:Analyze")
	// Other languages still read kinds from case.
	check(t, Python, "Point, MAX_SIZE", "type:Point", "constant:MAX_SIZE")
	check(t, Rust, "let p = Point::new(MAX);", "keyword:let", "type:Point", "function:new", "constant:MAX")
}

func TestRustStrings(t *testing.T) {
	check(t, Rust, `b"a" c"b" b'c' r"d\" r#"e"f"# br##"g"#h"## cr"i"`,
		`string:b"a"`, `string:c"b"`, `string:b'c'`, `string:r"d\"`, `string:r#"e"f"#`, `string:br##"g"#h"##`, `string:cr"i"`)
	check(t, Rust, "\"여러\n줄\" + 1", "string:\"여러\n줄\"", "number:1")
	check(t, Rust, `let r#type = 1;`, "keyword:let", "number:1")
	check(t, Rust, `r#"닫히지 않음`, `string:r#"닫히지 않음`)
}

func TestRustQuotes(t *testing.T) {
	check(t, Rust, `fn f<'a>(x: &'a str) -> char { 'x' }`,
		"keyword:fn", "function:f", "type:'a", "type:'a", "type:str", "type:char", "string:'x'")
	check(t, Rust, `'\n' '\'' '가' 'static`, `string:'\n'`, `string:'\''`, `string:'가'`, "type:'static")
}

func TestRustCommentsAndWords(t *testing.T) {
	check(t, Rust, "/* a /* b */ c */ x", "comment:/* a /* b */ c */")
	check(t, Rust, "/// 문서\n//! 모듈", "comment:/// 문서", "comment://! 모듈")
	check(t, Rust, `#[derive(Clone)] #![allow(x)]`, "attribute:#[derive", "type:Clone", "attribute:#![allow")
	check(t, Rust, `println!("a"); x != y; vec![1]`, "function:println", `string:"a"`, "function:vec", "number:1")
	check(t, Rust, `struct Foo; Foo(1); a.fn_x()`, "keyword:struct", "type:Foo", "type:Foo", "number:1", "function:fn_x")
	check(t, Rust, `x.0; a.0.1; self.0; f()?.0`, "number:0", "number:0.1", "keyword:self", "number:0", "function:f", "number:0")
	check(t, Rust, `0x00..0x04; 0xFFu32.max(1)`, "number:0x00", "number:0x04", "number:0xFFu32", "function:max", "number:1")
	check(t, Go, `0x1.8p3`, "number:0x1.8p3")
	check(t, Python, "return .5", "keyword:return", "number:.5")
	check(t, Rust, `0..10; 1.max(2); 1.5f64; 1u8; 0xFFu32`,
		"number:0", "number:10", "number:1", "function:max", "number:2", "number:1.5f64", "number:1u8", "number:0xFFu32")
}

func TestScriptRegularExpressions(t *testing.T) {
	check(t, TypeScript, `const x = /a\/[/]b/gi.test(y)`, "keyword:const", `string:/a\/[/]b/gi`, "function:test")
	check(t, TypeScript, `a / b / c`)
	check(t, TypeScript, `f(x) / 2; a[1] / 2`, "function:f", "number:2", "number:1", "number:2")
	check(t, TypeScript, `return /x/`, "keyword:return", "string:/x/")
	check(t, TypeScript, `a++ / 2; b / c`, "number:2")
	// Without a closing slash on the line, the slash divides.
	check(t, TypeScript, "= / 2\nx", "number:2")
	check(t, TypeScript, "this / 2 / 3", "constant:this", "number:2", "number:3")
}

func TestScriptTemplates(t *testing.T) {
	check(t, TypeScript, "`a${b}c`", "string:`a${", "string:}c`")
	check(t, TypeScript, "`a${f(`b${1}`)}c` + 2", "string:`a${", "function:f", "string:`b${", "number:1", "string:}`", "string:}c`", "number:2")
	check(t, TypeScript, "`a${ {x: 1}.x }c`", "string:`a${", "number:1", "string:}c`")
	check(t, TypeScript, "`\\${x}\n\\``", "string:`\\${x}\n\\``")
	check(t, TypeScript, "function g() { `${1}`; return 2 }",
		"keyword:function", "function:g", "string:`${", "number:1", "string:}`", "keyword:return", "number:2")
	check(t, TypeScript, "`닫히지 않음", "string:`닫히지 않음")
}

func TestScriptWords(t *testing.T) {
	check(t, TypeScript, "interface Foo { a: string }", "keyword:interface", "type:Foo", "type:string")
	check(t, TypeScript, "@Component({}) class A extends Base {}", "attribute:@Component", "keyword:class", "type:A", "keyword:extends", "type:Base")
	check(t, JavaScript, "x.from(1); from; $a.$b($c)", "function:from", "number:1", "keyword:from", "function:$b")
	check(t, JavaScript, "#!/usr/bin/env node\nnew Foo()", "comment:#!/usr/bin/env node", "keyword:new", "type:Foo")
	check(t, JavaScript, `"abc`+"\nx", `string:"abc`)
	check(t, JavaScript, "1n 0xFFn 1_000", "number:1n", "number:0xFFn", "number:1_000")
}

func TestTablesAreSortedAndWithoutOverlap(t *testing.T) {
	sources := []string{
		"def f():\n  return f'{a}' # x\n",
		"let x = `a${1}b${2}c`; /r/g; a/b/c",
		"#[a] fn f<'a>() { b'x'; r#\"y\"#; /* /* z */ */ }",
	}
	for _, src := range sources {
		for _, lang := range []*Language{Python, Go, Rust, JavaScript, TypeScript} {
			table := Lex(lang, []byte(src))
			end := uint32(0)
			for i := 0; i < table.Len(); i++ {
				if table.Starts[i] < end || table.Lens[i] == 0 {
					t.Errorf("%s %q: token %d overlaps", lang.Name, src, i)
				}
				end = table.Starts[i] + table.Lens[i]
			}
			if end > uint32(len(src)) {
				t.Errorf("%s %q: token past the end", lang.Name, src)
			}
		}
	}
}

func TestRangeFindsTokensTouchingBytes(t *testing.T) {
	src := "\"a\nb\" x 1 y 2"
	table := Lex(Rust, []byte(src))
	if table.Len() != 3 {
		t.Fatalf("%d tokens", table.Len())
	}
	for _, c := range []struct {
		start, end uint32
		from, to   int
	}{
		{0, uint32(len(src)), 0, 3},
		{2, 3, 0, 1}, // inside the two-line string
		{5, 7, 1, 1},
		{8, 9, 1, 2},
		{9, 11, 2, 2},
		{12, 13, 2, 3},
		{100, 200, 3, 3},
	} {
		from, to := table.Range(c.start, c.end)
		if from != c.from || to != c.to {
			t.Errorf("Range(%d, %d) = %d, %d; want %d, %d", c.start, c.end, from, to, c.from, c.to)
		}
	}
}

func TestDetectsLanguagesByExtension(t *testing.T) {
	for path, want := range map[string]*Language{
		`C:\a\b.py`: Python, "x.PYI": Python, "m.go": Go, "l.rs": Rust, "a.tsx": TypeScript, "a.mjs": JavaScript,
		"a.toml": nil, "Makefile": nil, "go": nil,
	} {
		if got := Detect(path); got != want {
			t.Errorf("%s: got %v", path, got)
		}
	}
}

// Odd input must end quickly and stay inside the text, whatever it holds.
func TestHandlesAnyBytesInLinearTime(t *testing.T) {
	inputs := [][]byte{
		bytes.Repeat([]byte("/"), 1<<16),
		bytes.Repeat([]byte("= /a "), 1<<14),
		bytes.Repeat([]byte("`${"), 1<<14),
		bytes.Repeat([]byte("}"), 1<<16),
		bytes.Repeat([]byte("'"), 1<<16),
		bytes.Repeat([]byte("f'{"), 1<<14),
		bytes.Repeat([]byte("/*"), 1<<16),
		bytes.Repeat([]byte("r#"), 1<<16),
		bytes.Repeat([]byte{0xff, 0xfe, 0x00, '\\'}, 1<<14),
		{'\\'}, {'\''}, {'"'}, {'`'}, {'#'}, {'r', '#'}, {'b'}, {'0', 'x'}, {'1', 'e'}, {'/'}, {'@'}, {'.'},
	}
	for _, input := range inputs {
		for _, lang := range []*Language{Python, Go, Rust, JavaScript, TypeScript} {
			table := Lex(lang, input)
			for i := 0; i < table.Len(); i++ {
				if int(table.Starts[i]+table.Lens[i]) > len(input) {
					t.Fatalf("%s: token past the end of %q", lang.Name, input[:min(len(input), 16)])
				}
			}
		}
	}
}

func BenchmarkLex(b *testing.B) {
	src := bytes.Repeat([]byte("def run(self, value: int) -> str:\n    # 주석입니다\n    return f\"{value!r}\" + 'x' * 42\n"), 1000)
	b.SetBytes(int64(len(src)))
	for range b.N {
		Lex(Python, src)
	}
}
