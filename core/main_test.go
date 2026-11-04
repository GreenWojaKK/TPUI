package main

import (
	"bufio"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// session runs serve over pipes.
type session struct {
	t     *testing.T
	in    *io.PipeWriter
	out   *bufio.Reader
	ended chan error
}

func start(t *testing.T) *session {
	t.Helper()
	inRead, inWrite := io.Pipe()
	outRead, outWrite := io.Pipe()
	s := &session{t: t, in: inWrite, out: bufio.NewReader(outRead), ended: make(chan error, 1)}
	go func() {
		s.ended <- serve(inRead, outWrite)
		outWrite.Close()
	}()
	t.Cleanup(func() { inWrite.Close() })
	return s
}

type reply struct {
	ID       uint64   `json:"id"`
	Language string   `json:"language"`
	Tokens   []uint32 `json:"tokens"`
	Stale    bool     `json:"stale"`
	Error    string   `json:"error"`
}

func (s *session) ask(req request) reply {
	s.t.Helper()
	line, err := json.Marshal(req)
	if err != nil {
		s.t.Fatal(err)
	}
	if _, err := s.in.Write(append(line, '\n')); err != nil {
		s.t.Fatal(err)
	}
	return s.read()
}

func (s *session) read() reply {
	s.t.Helper()
	line, err := s.out.ReadBytes('\n')
	if err != nil {
		s.t.Fatal(err)
	}
	var r reply
	if err := json.Unmarshal(line, &r); err != nil {
		s.t.Fatalf("%q: %v", line, err)
	}
	return r
}

func writeFile(t *testing.T, name, content string) (string, stamp) {
	t.Helper()
	path := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	at, err := statFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return path, at
}

func TestAnswersTokensOfARange(t *testing.T) {
	s := start(t)
	path, at := writeFile(t, "a.py", "def f():\n    return 1 # 끝\n")
	got := s.ask(request{ID: 7, Path: path, Stamp: at, Start: 0, End: 1 << 20})
	// def, f, return, 1 and the comment.
	if got.ID != 7 || got.Language != "Python" || len(got.Tokens) != 5*3 {
		t.Fatalf("%+v", got)
	}
	// def at byte 0, three bytes long, a keyword.
	if got.Tokens[0] != 0 || got.Tokens[1] != 3 || got.Tokens[2] != 4 {
		t.Errorf("first token: %v", got.Tokens[:3])
	}
	// The range touches return and the 1.
	only := s.ask(request{ID: 8, Path: path, Stamp: at, Start: 9, End: 21})
	if len(only.Tokens) != 2*3 || only.Tokens[0] != 13 {
		t.Errorf("range: %+v", only)
	}
}

func TestAnswersWithoutTokensForAFileWithoutRules(t *testing.T) {
	s := start(t)
	path, at := writeFile(t, "a.toml", "x = 1\n")
	got := s.ask(request{ID: 1, Path: path, Stamp: at, End: 100})
	if got.Language != "" || len(got.Tokens) != 0 || got.Error != "" || got.Stale {
		t.Fatalf("%+v", got)
	}
}

func TestDropsAByteOrderMark(t *testing.T) {
	s := start(t)
	path, at := writeFile(t, "a.go", "\xef\xbb\xbfpackage a")
	got := s.ask(request{ID: 1, Path: path, Stamp: at, End: 100})
	if got.Tokens[0] != 0 || got.Tokens[1] != 7 {
		t.Fatalf("%+v", got)
	}
}

func TestSaysStaleWhenTheFileDiffersFromTheViewers(t *testing.T) {
	s := start(t)
	path, at := writeFile(t, "a.py", "x = 1\n")
	if got := s.ask(request{ID: 1, Path: path, Stamp: at, End: 100}); got.Stale {
		t.Fatalf("%+v", got)
	}
	if err := os.WriteFile(path, []byte("x = 12\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	// The viewer still holds the old version, and it is still in memory: the answer stays consistent with it.
	if got := s.ask(request{ID: 2, Path: path, Stamp: at, End: 100}); got.Stale || len(got.Tokens) != 3 {
		t.Fatalf("%+v", got)
	}
	// Any other version than the one on disk is stale.
	other := stamp{Len: at.Len + 100, Modified: at.Modified}
	if got := s.ask(request{ID: 3, Path: path, Stamp: other, End: 100}); !got.Stale {
		t.Fatalf("%+v", got)
	}
	now, err := statFile(path)
	if err != nil {
		t.Fatal(err)
	}
	got := s.ask(request{ID: 4, Path: path, Stamp: now, End: 100})
	if got.Stale || got.Tokens[1] != 2 {
		t.Fatalf("%+v", got)
	}
}

func TestReportsMissingFiles(t *testing.T) {
	s := start(t)
	got := s.ask(request{ID: 1, Path: filepath.Join(t.TempDir(), "없음.py"), End: 10})
	if got.Error == "" {
		t.Fatalf("%+v", got)
	}
}

func TestEndsWhenInputCloses(t *testing.T) {
	s := start(t)
	path, at := writeFile(t, "a.py", "x\n")
	s.ask(request{ID: 1, Path: path, Stamp: at, End: 100})
	s.in.Close()
	select {
	case err := <-s.ended:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("serve did not end")
	}
}

func TestEndsOnABrokenProtocol(t *testing.T) {
	s := start(t)
	if _, err := s.in.Write([]byte("not json\n")); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-s.ended:
		if err == nil {
			t.Fatal("expected an error")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("serve did not end")
	}
}

func TestKeepsOnlyTheLatestWaitingRequest(t *testing.T) {
	box := newMailbox()
	box.put(request{ID: 1})
	box.put(request{ID: 2})
	if got := box.take(); got.ID != 2 {
		t.Fatalf("%+v", got)
	}
}
