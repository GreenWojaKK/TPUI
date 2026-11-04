// Command tpui-core is the viewer's code core. It reads source files and answers with their tokens, and looks only at
// the text of those files: it starts no other process and depends on no toolchain or interpreter.
//
// The viewer starts it and talks over stdin and stdout, one JSON object per line. A request names a file, the stamp the
// viewer read it at, and a byte range. An answer holds the tokens touching that range, or says why there are none.
// When stdin closes, the viewer is gone and the core ends, however the viewer ended. Only the latest waiting request is
// answered, since the viewer has moved on from the others.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"sync"

	"github.com/GreenWojaKK/TPUI/core/lex"
)

// maxLexBytes is the largest file that gets tokens. Larger files show as plain text.
const maxLexBytes = 256 << 20

type stamp struct {
	Len      int64 `json:"len"`
	Modified int64 `json:"modified"`
}

type request struct {
	ID    uint64 `json:"id"`
	Path  string `json:"path"`
	Stamp stamp  `json:"stamp"`
	Start uint32 `json:"start"`
	End   uint32 `json:"end"`
}

// activeFile is the one file whose tokens are held.
type activeFile struct {
	path  string
	stamp stamp
	// language is empty for a file without rules.
	language string
	table    *lex.Table
}

func statFile(path string) (stamp, error) {
	info, err := os.Stat(path)
	if err != nil {
		return stamp{}, err
	}
	return stamp{Len: info.Size(), Modified: info.ModTime().UnixNano()}, nil
}

// errStale says the file on disk is not the version the viewer has.
var errStale = fmt.Errorf("stale")

// load reads and scans a file at the given stamp, unless it is the active one already.
func (active *activeFile) load(path string, want stamp) error {
	if active.table != nil && active.path == path && active.stamp == want {
		return nil
	}
	active.table = nil
	before, err := statFile(path)
	if err != nil {
		return err
	}
	if before != want {
		return errStale
	}
	language := lex.Detect(path)
	table := &lex.Table{}
	name := ""
	if language != nil && before.Len <= maxLexBytes {
		src, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		after, err := statFile(path)
		if err != nil {
			return err
		}
		if after != want {
			return errStale
		}
		// Offsets count from after a byte order mark, as in the viewer.
		if len(src) >= 3 && src[0] == 0xef && src[1] == 0xbb && src[2] == 0xbf {
			src = src[3:]
		}
		table = lex.Lex(language, src)
		name = language.Name
	}
	*active = activeFile{path: path, stamp: want, language: name, table: table}
	return nil
}

// answer writes the response to one request as a line. A failure inside becomes an error answer.
func (active *activeFile) answer(w io.Writer, req request) (err error) {
	defer func() {
		if r := recover(); r != nil {
			active.table = nil
			err = writeJSON(w, map[string]any{"id": req.ID, "error": fmt.Sprint(r)})
		}
	}()
	switch loadErr := active.load(req.Path, req.Stamp); {
	case loadErr == errStale:
		return writeJSON(w, map[string]any{"id": req.ID, "stale": true})
	case loadErr != nil:
		return writeJSON(w, map[string]any{"id": req.ID, "error": loadErr.Error()})
	}
	return writeTokens(w, req, active)
}

func writeJSON(w io.Writer, value any) error {
	line, err := json.Marshal(value)
	if err != nil {
		return err
	}
	_, err = w.Write(append(line, '\n'))
	return err
}

// writeTokens writes tokens as a flat list of start, length and kind.
func writeTokens(w io.Writer, req request, active *activeFile) error {
	table := active.table
	from, to := table.Range(req.Start, req.End)
	language, err := json.Marshal(active.language)
	if err != nil {
		return err
	}
	line := make([]byte, 0, 64+(to-from)*16)
	line = fmt.Appendf(line, `{"id":%d,"language":%s,"tokens":[`, req.ID, language)
	for i := from; i < to; i++ {
		if i > from {
			line = append(line, ',')
		}
		line = fmt.Appendf(line, "%d,%d,%d", table.Starts[i], table.Lens[i], table.Kinds[i])
	}
	line = append(line, "]}\n"...)
	_, err = w.Write(line)
	return err
}

// mailbox holds the latest request not yet answered.
type mailbox struct {
	mu      sync.Mutex
	wake    *sync.Cond
	pending *request
}

func newMailbox() *mailbox {
	m := &mailbox{}
	m.wake = sync.NewCond(&m.mu)
	return m
}

func (m *mailbox) put(req request) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.pending = &req
	m.wake.Signal()
}

func (m *mailbox) take() request {
	m.mu.Lock()
	defer m.mu.Unlock()
	for m.pending == nil {
		m.wake.Wait()
	}
	req := *m.pending
	m.pending = nil
	return req
}

// serve answers requests from r on w until r ends.
func serve(r io.Reader, w io.Writer) error {
	box := newMailbox()
	out := bufio.NewWriter(w)
	failed := make(chan error, 1)
	go func() {
		active := &activeFile{}
		for {
			req := box.take()
			if err := active.answer(out, req); err != nil {
				failed <- err
				return
			}
			if err := out.Flush(); err != nil {
				failed <- err
				return
			}
		}
	}()
	lines := bufio.NewReader(r)
	type read struct {
		line []byte
		err  error
	}
	reads := make(chan read)
	go func() {
		for {
			line, err := lines.ReadBytes('\n')
			reads <- read{line, err}
			if err != nil {
				return
			}
		}
	}()
	for {
		select {
		case err := <-failed:
			return err
		case next := <-reads:
			if next.err != nil {
				if next.err == io.EOF {
					return nil
				}
				return next.err
			}
			var req request
			if err := json.Unmarshal(next.line, &req); err != nil {
				return fmt.Errorf("bad request: %w", err)
			}
			box.put(req)
		}
	}
}

func main() {
	if err := serve(os.Stdin, os.Stdout); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
