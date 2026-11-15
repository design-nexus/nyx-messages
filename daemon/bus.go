package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"sync"

	"github.com/rs/zerolog"
)

// Listen serves newline-delimited JSON on a Unix socket.
type Bus struct {
	ln     net.Listener
	core   *Core
	log    zerolog.Logger
	mu     sync.Mutex
	clients map[*client]struct{}
}

type client struct {
	conn net.Conn
	enc  *json.Encoder
	mu   sync.Mutex
}

func Listen(socket string, core *Core, log zerolog.Logger) (*Bus, error) {
	if err := os.MkdirAll(filepathDir(socket), 0o700); err != nil {
		return nil, err
	}
	_ = os.Remove(socket)
	ln, err := net.Listen("unix", socket)
	if err != nil {
		return nil, err
	}
	if err := os.Chmod(socket, 0o600); err != nil {
		_ = ln.Close()
		return nil, err
	}
	b := &Bus{
		ln:      ln,
		core:    core,
		log:     log,
		clients: make(map[*client]struct{}),
	}
	core.SetBroadcaster(b.Broadcast)
	go b.accept()
	return b, nil
}

func filepathDir(p string) string {
	for i := len(p) - 1; i >= 0; i-- {
		if p[i] == '/' {
			return p[:i]
		}
	}
	return "."
}

func (b *Bus) Close() {
	_ = b.ln.Close()
	b.mu.Lock()
	defer b.mu.Unlock()
	for c := range b.clients {
		_ = c.conn.Close()
	}
}

func (b *Bus) accept() {
	for {
		conn, err := b.ln.Accept()
		if err != nil {
			return
		}
		c := &client{conn: conn, enc: json.NewEncoder(conn)}
		b.mu.Lock()
		b.clients[c] = struct{}{}
		b.mu.Unlock()
		go b.serve(c)
	}
}

func (b *Bus) serve(c *client) {
	defer func() {
		b.mu.Lock()
		delete(b.clients, c)
		b.mu.Unlock()
		_ = c.conn.Close()
	}()
	sc := bufio.NewScanner(c.conn)
	sc.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	for sc.Scan() {
		line := sc.Bytes()
		if len(line) == 0 {
			continue
		}
		var req Request
		if err := json.Unmarshal(line, &req); err != nil {
			_ = c.write(Response{OK: false, Error: "bad json: " + err.Error()})
			continue
		}
		res := b.core.Handle(req)
		res.ID = req.ID
		if err := c.write(res); err != nil {
			return
		}
	}
}

func (c *client) write(v any) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.enc.Encode(v)
}

// Broadcast sends an async event to every connected client.
func (b *Bus) Broadcast(evt Event) {
	b.mu.Lock()
	clients := make([]*client, 0, len(b.clients))
	for c := range b.clients {
		clients = append(clients, c)
	}
	b.mu.Unlock()
	for _, c := range clients {
		if err := c.write(evt); err != nil {
			b.log.Debug().Err(err).Msg("broadcast write failed")
		}
	}
}

// Request is a client → daemon command.
type Request struct {
	ID       string            `json:"id"`
	Cmd      string            `json:"cmd"`
	Cookies  map[string]string `json:"cookies,omitempty"`
	ChatID   string            `json:"chat_id,omitempty"`
	Text     string            `json:"text,omitempty"`
	MsgID    string            `json:"message_id,omitempty"`
	Count    int               `json:"count,omitempty"`
	ForceNew bool              `json:"force_new,omitempty"`
	Phone    string            `json:"phone,omitempty"`
	Path     string            `json:"path,omitempty"`
	MediaID  string            `json:"media_id,omitempty"`
	KeyB64   string            `json:"key_b64,omitempty"`
	Name     string            `json:"name,omitempty"`
	Emoji    string            `json:"emoji,omitempty"`
	Typing   bool              `json:"typing,omitempty"`
	Remove   bool              `json:"remove,omitempty"`
}

// Response is a daemon → client reply to a request.
type Response struct {
	ID     string `json:"id,omitempty"`
	OK     bool   `json:"ok"`
	Error  string `json:"error,omitempty"`
	Result any    `json:"result,omitempty"`
}

// Event is a daemon → client push (no id).
type Event struct {
	Event   string `json:"event"`
	Emoji   string `json:"emoji,omitempty"`
	State   string `json:"state,omitempty"`
	Chat    *Chat  `json:"chat,omitempty"`
	Message *Msg   `json:"message,omitempty"`
	Account string `json:"account,omitempty"`
	Error   string `json:"error,omitempty"`
	ChatID  string `json:"chat_id,omitempty"`
	Typing  bool   `json:"typing,omitempty"`
}

type Chat struct {
	ID        string `json:"id"`
	Name      string `json:"name"`
	Snippet   string `json:"snippet,omitempty"`
	Timestamp int64  `json:"timestamp_ms,omitempty"`
	Unread    bool   `json:"unread"`
	IsGroup   bool   `json:"is_group"`
	Pinned    bool   `json:"pinned"`
	Type      string `json:"type,omitempty"` // sms | rcs | unknown
}

type Attachment struct {
	ID     string `json:"id"`
	Name   string `json:"name,omitempty"`
	Mime   string `json:"mime,omitempty"`
	Size   int64  `json:"size,omitempty"`
	KeyB64 string `json:"key_b64,omitempty"`
}

type Reaction struct {
	Emoji  string `json:"emoji"`
	Count  int    `json:"count"`
	FromMe bool   `json:"from_me,omitempty"`
}

type Msg struct {
	ID          string       `json:"id"`
	ChatID      string       `json:"chat_id"`
	Text        string       `json:"text"`
	FromMe      bool         `json:"from_me"`
	Sender      string       `json:"sender,omitempty"`
	Timestamp   int64        `json:"timestamp_ms"`
	TmpID       string       `json:"tmp_id,omitempty"`
	Status      string       `json:"status,omitempty"`
	Attachments []Attachment `json:"attachments,omitempty"`
	Reactions   []Reaction   `json:"reactions,omitempty"`
}

func (b *Bus) String() string { return fmt.Sprintf("bus(%d clients)", len(b.clients)) }
