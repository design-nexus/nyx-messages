package main

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/events"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

const pairTimeout = 5 * time.Minute

type Core struct {
	paths Paths
	log   zerolog.Logger

	mu     sync.Mutex
	auth   *libgm.AuthData
	client *libgm.Client
	chats  map[string]*Chat
	msgs   map[string][]*Msg // by chat id, newest last
	conn   string            // disconnected | connecting | connected | needs_repair
	account string

	// Live GET_UPDATES can populate chats/msgs before the UI asks for a full
	// list. Without these flags we'd skip ListConversations / FetchMessages
	// and only show the few live rows that arrived after a restart.
	inboxSynced bool
	msgSynced   map[string]bool

	broadcast func(Event)

	pairCancel context.CancelFunc
}

func NewCore(paths Paths, log zerolog.Logger) *Core {
	return &Core{
		paths:     paths,
		log:       log,
		chats:     make(map[string]*Chat),
		msgs:      make(map[string][]*Msg),
		msgSynced: make(map[string]bool),
		conn:      "disconnected",
	}
}

func (c *Core) SetBroadcaster(fn func(Event)) {
	c.mu.Lock()
	c.broadcast = fn
	c.mu.Unlock()
}

func (c *Core) emit(evt Event) {
	c.mu.Lock()
	fn := c.broadcast
	c.mu.Unlock()
	if fn != nil {
		fn(evt)
	}
}

func (c *Core) Boot() error {
	if err := c.paths.ensureDirs(); err != nil {
		return err
	}
	auth, err := loadAuth(c.paths.Session)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}
	if auth.Browser == nil {
		return nil
	}
	c.auth = auth
	return c.connectLocked()
}

func (c *Core) Shutdown() {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.pairCancel != nil {
		c.pairCancel()
		c.pairCancel = nil
	}
	if c.client != nil {
		c.client.Disconnect()
		c.client = nil
	}
	c.conn = "disconnected"
}

func (c *Core) Handle(req Request) Response {
	switch req.Cmd {
	case "status":
		return c.status()
	case "pair.start":
		return c.pairStart(req)
	case "pair.cancel":
		return c.pairCancelCmd()
	case "logout":
		return c.logout()
	case "chats.list":
		return c.chatsList()
	case "chats.get_or_create":
		return c.chatsGetOrCreate(req)
	case "messages.list":
		return c.messagesList(req)
	case "messages.send":
		return c.messagesSend(req)
	case "messages.mark_read":
		return c.messagesMarkRead(req)
	case "media.download":
		return c.mediaDownload(req)
	case "typing.set":
		return c.typingSet(req)
	case "messages.react":
		return c.messagesReact(req)
	default:
		return Response{OK: false, Error: "unknown cmd: " + req.Cmd}
	}
}

func (c *Core) status() Response {
	c.mu.Lock()
	defer c.mu.Unlock()
	paired := c.auth != nil && c.auth.Browser != nil
	return Response{OK: true, Result: map[string]any{
		"paired":    paired,
		"connected": c.conn == "connected",
		"state":     c.conn,
		"account":   c.account,
	}}
}

func (c *Core) pairCancelCmd() Response {
	c.mu.Lock()
	cancel := c.pairCancel
	c.mu.Unlock()
	if cancel != nil {
		cancel()
	}
	return Response{OK: true}
}

func (c *Core) logout() Response {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.pairCancel != nil {
		c.pairCancel()
		c.pairCancel = nil
	}
	if c.client != nil {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		_ = c.client.UnpairGaia(ctx)
		cancel()
		c.client.Disconnect()
		c.client = nil
	}
	c.auth = nil
	c.chats = make(map[string]*Chat)
	c.msgs = make(map[string][]*Msg)
	c.msgSynced = make(map[string]bool)
	c.inboxSynced = false
	c.account = ""
	c.conn = "disconnected"
	_ = removeAuth(c.paths.Session)
	c.emitUnlocked(Event{Event: "connection", State: "disconnected"})
	return Response{OK: true}
}

func (c *Core) emitUnlocked(evt Event) {
	fn := c.broadcast
	if fn != nil {
		// unlock not held for broadcast — caller may hold mu; copy fn only
		go fn(evt)
	}
}

func (c *Core) pairStart(req Request) Response {
	if len(req.Cookies) == 0 {
		return Response{OK: false, Error: "cookies required"}
	}
	needed := []string{"SID", "HSID", "SSID", "OSID", "APISID", "SAPISID"}
	for _, k := range needed {
		if req.Cookies[k] == "" {
			return Response{OK: false, Error: "missing cookie: " + k}
		}
	}

	c.mu.Lock()
	if c.pairCancel != nil {
		c.pairCancel()
		c.pairCancel = nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), pairTimeout)
	c.pairCancel = cancel
	forceNew := req.ForceNew
	cookies := cloneMap(req.Cookies)
	c.mu.Unlock()

	emojiReady := make(chan map[string]any, 1)
	earlyFail := make(chan Response, 1)
	go func() {
		res := c.runPair(ctx, cookies, forceNew, emojiReady)
		c.mu.Lock()
		c.pairCancel = nil
		c.mu.Unlock()
		cancel()
		if res.OK {
			return
		}
		c.emit(Event{Event: "pair.fail", Error: res.Error})
		select {
		case earlyFail <- res:
		default:
		}
	}()
	select {
	case result := <-emojiReady:
		return Response{OK: true, Result: result}
	case res := <-earlyFail:
		return res
	case <-ctx.Done():
		return Response{OK: false, Error: "pairing timed out waiting for phone"}
	}
}

func (c *Core) runPair(ctx context.Context, cookies map[string]string, forceNew bool, emojiReady chan<- map[string]any) Response {
	// Reauth existing Gaia session when possible.
	if !forceNew {
		if auth, err := loadAuth(c.paths.Session); err == nil && auth.IsGoogleAccount() && auth.Browser != nil {
			auth.SetCookies(cookies)
			cli := libgm.NewClient(auth, nil, c.log)
			defer cli.Disconnect()
			if err := cli.FetchConfig(ctx); err == nil {
				email := ""
				if cli.Config != nil {
					email = cli.Config.GetDeviceInfo().GetEmail()
				}
				if email != "" {
					if err := saveAuth(c.paths.Session, auth); err == nil {
						c.mu.Lock()
						c.auth = auth
						c.account = email
						c.mu.Unlock()
						if err := c.connectLocked(); err != nil {
							return Response{OK: false, Error: err.Error()}
						}
						c.emit(Event{Event: "pair.ok", Account: email})
						result := map[string]any{
							"mode":    "reauthenticated",
							"account": email,
						}
						select {
						case emojiReady <- result:
						default:
						}
						return Response{OK: true, Result: result}
					}
				}
			}
			cli.Disconnect()
		}
	}

	auth := libgm.NewAuthData()
	auth.SetCookies(cookies)
	cli := libgm.NewClient(auth, nil, c.log)
	defer cli.Disconnect()

	if err := cli.FetchConfig(ctx); err != nil {
		return Response{OK: false, Error: "validate cookies: " + err.Error()}
	}
	account := ""
	if cli.Config != nil {
		account = cli.Config.GetDeviceInfo().GetEmail()
	}
	if account == "" {
		return Response{OK: false, Error: "Google Messages config did not identify an account"}
	}

	type startRes struct {
		emoji string
		ps    *libgm.PairingSession
		err   error
	}
	started := make(chan startRes, 1)
	go func() {
		emoji, ps, err := cli.StartGaiaPairing(ctx)
		started <- startRes{emoji, ps, err}
	}()
	var sr startRes
	select {
	case sr = <-started:
	case <-ctx.Done():
		return Response{OK: false, Error: "pairing timed out waiting for phone"}
	}
	if sr.err != nil {
		return Response{OK: false, Error: describePairErr("start", sr.err)}
	}
	c.emit(Event{Event: "pair.emoji", Emoji: sr.emoji, Account: account})
	select {
	case emojiReady <- map[string]any{
		"mode":    "awaiting_confirm",
		"emoji":   sr.emoji,
		"account": account,
	}:
	default:
	}

	phoneID, err := cli.FinishGaiaPairing(ctx, sr.ps)
	if err != nil {
		return Response{OK: false, Error: describePairErr("finish", err)}
	}
	if err := saveAuth(c.paths.Session, auth); err != nil {
		return Response{OK: false, Error: "persist session: " + err.Error()}
	}

	c.mu.Lock()
	c.auth = auth
	c.account = account
	c.mu.Unlock()

	if err := c.connectLocked(); err != nil {
		return Response{OK: false, Error: "connect after pair: " + err.Error()}
	}
	c.emit(Event{Event: "pair.ok", Account: account})
	return Response{OK: true, Result: map[string]any{
		"mode":     "paired",
		"account":  account,
		"phone_id": phoneID,
	}}
}

func describePairErr(stage string, err error) string {
	switch {
	case errors.Is(err, libgm.ErrNoDevicesFound):
		return stage + ": no phone has Google Account pairing enabled — open Messages → Device pairing"
	case errors.Is(err, libgm.ErrPairingInitTimeout):
		return stage + ": phone did not respond — keep Messages open"
	case errors.Is(err, libgm.ErrIncorrectEmoji):
		return stage + ": wrong emoji selected on the phone"
	case errors.Is(err, libgm.ErrPairingCancelled):
		return stage + ": cancelled on the phone"
	case errors.Is(err, libgm.ErrPairingTimeout), errors.Is(err, context.DeadlineExceeded):
		return stage + ": confirmation timed out"
	default:
		return stage + ": " + err.Error()
	}
}

func (c *Core) connectLocked() error {
	c.mu.Lock()
	auth := c.auth
	if auth == nil || auth.Browser == nil {
		c.mu.Unlock()
		return errors.New("not paired")
	}
	if c.client != nil {
		c.client.Disconnect()
		c.client = nil
	}
	c.conn = "connecting"
	c.mu.Unlock()
	c.emit(Event{Event: "connection", State: "connecting"})

	cli := libgm.NewClient(auth, nil, c.log)
	cli.SetEventHandler(c.onEvent)
	if err := cli.Connect(); err != nil {
		c.mu.Lock()
		c.conn = "needs_repair"
		c.mu.Unlock()
		c.emit(Event{Event: "connection", State: "needs_repair", Error: err.Error()})
		c.emit(Event{Event: "session.needs_repair", Error: err.Error()})
		return err
	}
	c.mu.Lock()
	c.client = cli
	c.conn = "connected"
	c.mu.Unlock()
	c.emit(Event{Event: "connection", State: "connected"})
	return nil
}

func (c *Core) onEvent(evt any) {
	switch e := evt.(type) {
	case *events.ClientReady:
		// Quiet: one chat.upsert per conversation freezes the GTK client.
		for _, conv := range e.Conversations {
			c.storeConversation(conv)
		}
		c.mu.Lock()
		c.conn = "connected"
		c.mu.Unlock()
		c.emit(Event{Event: "connection", State: "connected"})
		c.emit(Event{Event: "chats.refresh"})
	case *gmproto.Conversation:
		c.upsertConversation(e)
	case *libgm.WrappedMessage:
		c.upsertMessage(e)
	case *events.AuthTokenRefreshed:
		c.mu.Lock()
		auth := c.auth
		c.mu.Unlock()
		if auth != nil {
			_ = saveAuth(c.paths.Session, auth)
		}
	case *events.PairSuccessful:
		c.mu.Lock()
		auth := c.auth
		c.mu.Unlock()
		if auth != nil {
			_ = saveAuth(c.paths.Session, auth)
		}
	case *events.PhoneNotResponding:
		c.emit(Event{Event: "connection", State: "connecting"})
	case *events.PhoneRespondingAgain, *events.ListenRecovered:
		c.mu.Lock()
		c.conn = "connected"
		c.mu.Unlock()
		c.emit(Event{Event: "connection", State: "connected"})
	case *events.GaiaLoggedOut:
		c.mu.Lock()
		c.conn = "needs_repair"
		c.mu.Unlock()
		c.emit(Event{Event: "session.needs_repair", Error: "Google Account logged out"})
		c.emit(Event{Event: "connection", State: "needs_repair"})
	case *events.ListenFatalError:
		msg := "listen failed"
		if e.Error != nil {
			msg = e.Error.Error()
		}
		c.mu.Lock()
		c.conn = "needs_repair"
		c.mu.Unlock()
		c.emit(Event{Event: "session.needs_repair", Error: msg})
		c.emit(Event{Event: "connection", State: "needs_repair", Error: msg})
	case *events.ListenTemporaryError:
		c.log.Warn().Err(e.Error).Msg("temporary listen error")
	case *gmproto.TypingData:
		if e == nil || e.GetConversationID() == "" {
			return
		}
		c.emit(Event{
			Event:  "typing",
			ChatID:  e.GetConversationID(),
			Typing: e.GetType() == gmproto.TypingTypes_STARTED_TYPING,
		})
	}
}

func (c *Core) storeConversation(conv *gmproto.Conversation) *Chat {
	if conv == nil || conv.GetConversationID() == "" {
		return nil
	}
	chat := chatFromProto(conv)
	c.mu.Lock()
	// ListConversations / conversation updates often carry a stale
	// LastMessageTimestamp. Never let that shove a chat we've already
	// advanced (via message.upsert) back down the inbox.
	if existing, ok := c.chats[chat.ID]; ok {
		if existing.Timestamp > chat.Timestamp {
			chat.Timestamp = existing.Timestamp
			if existing.Snippet != "" {
				chat.Snippet = existing.Snippet
			}
		}
		// Keep unread sticky if Google's row cleared it but we still flag it.
		if existing.Unread && !chat.Unread {
			chat.Unread = true
		}
	}
	c.chats[chat.ID] = chat
	c.mu.Unlock()
	return chat
}

func (c *Core) upsertConversation(conv *gmproto.Conversation) {
	chat := c.storeConversation(conv)
	if chat != nil {
		c.emit(Event{Event: "chat.upsert", Chat: chat})
	}
}

func (c *Core) upsertMessage(w *libgm.WrappedMessage) {
	if w == nil || w.Message == nil {
		return
	}
	m := msgFromProto(w.Message)
	if m.ID == "" || m.ChatID == "" {
		return
	}
	// Drop Google's "Texting with … (SMS/MMS)" / "RCS chat with …" notices.
	if isProtocolNotice(m.Text) {
		return
	}
	c.mu.Lock()
	list := c.msgs[m.ChatID]
	replaced := false
	for i, existing := range list {
		if existing.ID == m.ID || (m.TmpID != "" && existing.TmpID == m.TmpID) {
			list[i] = m
			replaced = true
			break
		}
	}
	if !replaced {
		list = append(list, m)
	}
	c.msgs[m.ChatID] = list
	if chat, ok := c.chats[m.ChatID]; ok {
		if !isProtocolNotice(m.Text) {
			chat.Snippet = m.Text
		}
		chat.Timestamp = m.Timestamp
		if !m.FromMe {
			chat.Unread = true
		}
	}
	c.mu.Unlock()
	// Historical FetchMessages must not push one event per row — that locks the UI.
	if w.IsOld {
		return
	}
	c.emit(Event{Event: "message.upsert", Message: m})
}

func chatFromProto(conv *gmproto.Conversation) *Chat {
	snippet := ""
	if lm := conv.GetLatestMessage(); lm != nil {
		snippet = lm.GetDisplayContent()
	}
	if isProtocolNotice(snippet) {
		snippet = ""
	}
	typ := "unknown"
	switch conv.GetType() {
	case gmproto.ConversationType_SMS:
		typ = "sms"
	case gmproto.ConversationType_RCS:
		typ = "rcs"
	}
	name := strings.TrimSpace(conv.GetName())
	participantName := participantDisplayName(conv)
	// Google often puts "Texting with … (SMS/MMS)" / "RCS chat with …" in Name.
	if name == "" || isProtocolNotice(name) || looksLikeProtocolTitle(name) {
		name = participantName
	}
	return &Chat{
		ID:        conv.GetConversationID(),
		Name:      name,
		Snippet:   snippet,
		Timestamp: normalizeTS(conv.GetLastMessageTimestamp()),
		Unread:    conv.GetUnread(),
		IsGroup:   conv.GetIsGroupChat(),
		Pinned:    conv.GetPinned(),
		Type:      typ,
	}
}

func participantDisplayName(conv *gmproto.Conversation) string {
	parts := conv.GetParticipants()
	var names []string
	for _, p := range parts {
		if p == nil || p.GetIsMe() {
			continue
		}
		n := p.GetFullName()
		if n == "" {
			n = p.GetFormattedNumber()
		}
		if n == "" {
			if id := p.GetID(); id != nil {
				n = id.GetNumber()
				if n == "" {
					n = id.GetParticipantID()
				}
			}
		}
		if n != "" {
			names = append(names, n)
		}
	}
	return strings.Join(names, ", ")
}

// isProtocolNotice matches Google Messages auto-labels that aren't real chat text.
func isProtocolNotice(s string) bool {
	t := strings.TrimSpace(strings.ToLower(s))
	if t == "" {
		return false
	}
	if strings.HasPrefix(t, "texting with ") {
		return true
	}
	if strings.HasPrefix(t, "rcs chat with ") {
		return true
	}
	if strings.Contains(t, "(sms/mms)") && strings.HasPrefix(t, "texting ") {
		return true
	}
	return false
}

func looksLikeProtocolTitle(s string) bool {
	t := strings.TrimSpace(strings.ToLower(s))
	return t == "sms" || t == "mms" || t == "sms/mms" || t == "rcs" || t == "chat"
}

func msgStatusString(st gmproto.MessageStatusType) string {
	switch st {
	case gmproto.MessageStatusType_OUTGOING_SENDING,
		gmproto.MessageStatusType_OUTGOING_YET_TO_SEND,
		gmproto.MessageStatusType_OUTGOING_RESENDING,
		gmproto.MessageStatusType_OUTGOING_AWAITING_RETRY,
		gmproto.MessageStatusType_OUTGOING_VALIDATING:
		return "sending"
	case gmproto.MessageStatusType_OUTGOING_COMPLETE,
		gmproto.MessageStatusType_OUTGOING_NOT_DELIVERED_YET:
		return "sent"
	case gmproto.MessageStatusType_OUTGOING_DELIVERED:
		return "delivered"
	case gmproto.MessageStatusType_OUTGOING_DISPLAYED:
		return "read"
	case gmproto.MessageStatusType_OUTGOING_FAILED_GENERIC,
		gmproto.MessageStatusType_OUTGOING_FAILED_EMERGENCY_NUMBER,
		gmproto.MessageStatusType_OUTGOING_FAILED_TOO_LARGE,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_LOST_RCS,
		gmproto.MessageStatusType_OUTGOING_FAILED_NO_RETRY_NO_FALLBACK,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_DID_NOT_DECRYPT,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_LOST_ENCRYPTION,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_DID_NOT_DECRYPT_NO_MORE_RETRY,
		gmproto.MessageStatusType_OUTGOING_FAILED_RECIPIENT_NEGATIVE_DELIVERY,
		gmproto.MessageStatusType_OUTGOING_FAILED_TO_ENCRYPT,
		gmproto.MessageStatusType_OUTGOING_CANCELED:
		return "failed"
	default:
		return ""
	}
}

func msgFromProto(m *gmproto.Message) *Msg {
	text := ""
	var atts []Attachment
	for _, info := range m.GetMessageInfo() {
		if info == nil {
			continue
		}
		if mc := info.GetMessageContent(); mc != nil && text == "" {
			text = mc.GetContent()
		}
		if media := info.GetMediaContent(); media != nil && media.GetMediaID() != "" {
			atts = append(atts, Attachment{
				ID:     media.GetMediaID(),
				Name:   media.GetMediaName(),
				Mime:   media.GetMimeType(),
				Size:   media.GetSize(),
				KeyB64: base64.StdEncoding.EncodeToString(media.GetDecryptionKey()),
			})
		}
	}
	fromMe := false
	status := ""
	if p := m.GetSenderParticipant(); p != nil && p.GetIsMe() {
		fromMe = true
	}
	if st := m.GetMessageStatus(); st != nil {
		status = msgStatusString(st.GetStatus())
		if !fromMe {
			switch st.GetStatus() {
			case gmproto.MessageStatusType_OUTGOING_COMPLETE,
				gmproto.MessageStatusType_OUTGOING_DELIVERED,
				gmproto.MessageStatusType_OUTGOING_DISPLAYED,
				gmproto.MessageStatusType_OUTGOING_SENDING,
				gmproto.MessageStatusType_OUTGOING_YET_TO_SEND:
				fromMe = true
			}
		}
	}
	if !fromMe {
		status = ""
	}
	sender := ""
	if p := m.GetSenderParticipant(); p != nil {
		sender = p.GetFullName()
		if sender == "" {
			sender = p.GetFormattedNumber()
		}
	}
	var reactions []Reaction
	for _, re := range m.GetReactions() {
		if re == nil || re.GetData() == nil {
			continue
		}
		emoji := re.GetData().GetUnicode()
		if emoji == "" {
			continue
		}
		reactions = append(reactions, Reaction{
			Emoji: emoji,
			Count: len(re.GetParticipantIDs()),
		})
	}
	return &Msg{
		ID:          m.GetMessageID(),
		ChatID:      m.GetConversationID(),
		Text:        text,
		FromMe:      fromMe,
		Sender:      sender,
		Timestamp:   normalizeTS(m.GetTimestamp()),
		TmpID:       m.GetTmpID(),
		Status:      status,
		Attachments: atts,
		Reactions:   reactions,
	}
}

func normalizeTS(ts int64) int64 {
	if ts <= 0 {
		return 0
	}
	// libgm timestamps are often microseconds.
	if ts > 1_000_000_000_000_000 {
		return ts / 1000
	}
	if ts < 1_000_000_000_000 {
		return ts * 1000
	}
	return ts
}

func normalizePhoneDigits(s string) string {
	var b strings.Builder
	for _, r := range s {
		if r >= '0' && r <= '9' {
			b.WriteRune(r)
		}
	}
	return b.String()
}

func phoneDigitsMatch(a, b string) bool {
	if a == "" || b == "" {
		return false
	}
	if a == b {
		return true
	}
	a10, b10 := a, b
	if len(a) >= 10 {
		a10 = a[len(a)-10:]
	}
	if len(b) >= 10 {
		b10 = b[len(b)-10:]
	}
	return a10 == b10
}

func (c *Core) findChatByPhoneLocked(digits string) *Chat {
	if len(digits) < 7 {
		return nil
	}
	for _, ch := range c.chats {
		if ch.IsGroup {
			continue
		}
		if phoneDigitsMatch(digits, normalizePhoneDigits(ch.Name)) ||
			phoneDigitsMatch(digits, normalizePhoneDigits(ch.ID)) {
			cp := *ch
			return &cp
		}
	}
	return nil
}

func (c *Core) chatsGetOrCreate(req Request) Response {
	phone := strings.TrimSpace(req.Phone)
	if phone == "" {
		return Response{OK: false, Error: "phone required"}
	}
	digits := normalizePhoneDigits(phone)
	if len(digits) < 7 {
		return Response{OK: false, Error: "phone number too short"}
	}
	// Prefer E.164-ish form when the user typed digits only.
	normalized := phone
	if !strings.HasPrefix(phone, "+") && len(digits) >= 10 {
		normalized = "+" + digits
	}

	c.mu.Lock()
	if existing := c.findChatByPhoneLocked(digits); existing != nil {
		c.mu.Unlock()
		return Response{OK: true, Result: map[string]any{"chat": existing}}
	}
	cli := c.client
	c.mu.Unlock()
	if cli == nil {
		return Response{OK: false, Error: "not connected"}
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	resp, err := cli.GetOrCreateConversation(ctx, &gmproto.GetOrCreateConversationRequest{
		Numbers: []*gmproto.ContactNumber{{
			MysteriousInt: 2,
			Number:        normalized,
			Number2:       normalized,
		}},
	})
	if err != nil {
		return Response{OK: false, Error: err.Error()}
	}
	switch resp.GetStatus() {
	case gmproto.GetOrCreateConversationResponse_SUCCESS,
		gmproto.GetOrCreateConversationResponse_UNKNOWN:
		// ok
	case gmproto.GetOrCreateConversationResponse_CREATE_RCS:
		return Response{OK: false, Error: "couldn't start chat (RCS group create required)"}
	default:
		return Response{OK: false, Error: fmt.Sprintf("couldn't start chat (status %v)", resp.GetStatus())}
	}
	conv := resp.GetConversation()
	if conv == nil || conv.GetConversationID() == "" {
		return Response{OK: false, Error: "no conversation returned"}
	}
	chat := c.storeConversation(conv)
	if chat == nil {
		return Response{OK: false, Error: "failed to store conversation"}
	}
	// Prefer the number the user typed when Google left the title empty/protocol-y.
	if chat.Name == "" || isProtocolNotice(chat.Name) || looksLikeProtocolTitle(chat.Name) {
		chat.Name = phone
	}
	c.emit(Event{Event: "chat.upsert", Chat: chat})
	cp := *chat
	return Response{OK: true, Result: map[string]any{"chat": &cp}}
}

func (c *Core) chatsList() Response {
	c.mu.Lock()
	cli := c.client
	conn := c.conn
	needSync := !c.inboxSynced
	c.mu.Unlock()

	if needSync {
		if cli == nil {
			// Don't lie with an empty inbox while libgm is still coming up.
			if conn == "connecting" || conn == "disconnected" {
				return Response{OK: false, Error: "connecting"}
			}
			return Response{OK: false, Error: "not connected"}
		}
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		resp, err := cli.ListConversations(ctx, 50, gmproto.ListConversationsRequest_INBOX)
		if err != nil {
			return Response{OK: false, Error: err.Error()}
		}
		for _, conv := range resp.GetConversations() {
			c.storeConversation(conv)
		}
		c.mu.Lock()
		c.inboxSynced = true
		c.mu.Unlock()
	}

	c.mu.Lock()
	list := make([]*Chat, 0, len(c.chats))
	for _, ch := range c.chats {
		// Reconcile from cached messages so a stale Conversation row can't
		// bury a chat we already have newer texts for.
		if msgs := c.msgs[ch.ID]; len(msgs) > 0 {
			for _, m := range msgs {
				if m == nil {
					continue
				}
				if m.Timestamp > ch.Timestamp {
					ch.Timestamp = m.Timestamp
					if !isProtocolNotice(m.Text) && m.Text != "" {
						ch.Snippet = m.Text
					}
				}
			}
		}
		cp := *ch
		list = append(list, &cp)
	}
	c.mu.Unlock()

	sort.Slice(list, func(i, j int) bool {
		return list[i].Timestamp > list[j].Timestamp
	})
	return Response{OK: true, Result: map[string]any{"chats": list}}
}

func (c *Core) messagesList(req Request) Response {
	if req.ChatID == "" {
		return Response{OK: false, Error: "chat_id required"}
	}
	count := req.Count
	if count <= 0 {
		count = 50
	}

	c.mu.Lock()
	cli := c.client
	needFetch := !c.msgSynced[req.ChatID]
	c.mu.Unlock()

	if needFetch && cli != nil {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		resp, err := cli.FetchMessages(ctx, req.ChatID, int64(count), nil)
		if err != nil {
			return Response{OK: false, Error: err.Error()}
		}
		for _, m := range resp.GetMessages() {
			c.upsertMessage(&libgm.WrappedMessage{Message: m, IsOld: true})
		}
		c.mu.Lock()
		c.msgSynced[req.ChatID] = true
		c.mu.Unlock()
	}

	c.mu.Lock()
	cached := append([]*Msg(nil), c.msgs[req.ChatID]...)
	c.mu.Unlock()

	sort.Slice(cached, func(i, j int) bool {
		return cached[i].Timestamp < cached[j].Timestamp
	})
	out := make([]*Msg, 0, len(cached))
	for _, m := range cached {
		if m != nil && isProtocolNotice(m.Text) {
			continue
		}
		out = append(out, m)
	}
	if len(out) > count {
		out = out[len(out)-count:]
	}
	return Response{OK: true, Result: map[string]any{"messages": out}}
}

func (c *Core) messagesSend(req Request) Response {
	text := strings.TrimSpace(req.Text)
	if req.ChatID == "" || (text == "" && req.Path == "") {
		return Response{OK: false, Error: "chat_id and text or path required"}
	}
	c.mu.Lock()
	cli := c.client
	c.mu.Unlock()
	if cli == nil {
		return Response{OK: false, Error: "not connected"}
	}

	var media *gmproto.MediaContent
	var atts []Attachment
	if req.Path != "" {
		data, err := os.ReadFile(req.Path)
		if err != nil {
			return Response{OK: false, Error: "read file: " + err.Error()}
		}
		name := filepath.Base(req.Path)
		mime := mimeFromName(name)
		media, err = cli.UploadMedia(data, name, mime)
		if err != nil {
			return Response{OK: false, Error: "upload: " + err.Error()}
		}
		atts = []Attachment{{
			ID:     media.GetMediaID(),
			Name:   name,
			Mime:   mime,
			Size:   int64(len(data)),
			KeyB64: base64.StdEncoding.EncodeToString(media.GetDecryptionKey()),
		}}
	}

	tmpID := uuid.NewString()
	infos := make([]*gmproto.MessageInfo, 0, 2)
	if text != "" {
		infos = append(infos, &gmproto.MessageInfo{
			Data: &gmproto.MessageInfo_MessageContent{
				MessageContent: &gmproto.MessageContent{Content: text},
			},
		})
	}
	if media != nil {
		infos = append(infos, &gmproto.MessageInfo{
			Data: &gmproto.MessageInfo_MediaContent{MediaContent: media},
		})
	}
	payload := &gmproto.SendMessageRequest{
		ConversationID: req.ChatID,
		TmpID:          tmpID,
		MessagePayload: &gmproto.MessagePayload{
			ConversationID: req.ChatID,
			TmpID:          tmpID,
			TmpID2:         tmpID,
			MessageInfo:    infos,
		},
	}
	// Keep legacy single MessageContent field populated when text-only (older phones).
	if text != "" && media == nil {
		payload.MessagePayload.MessagePayloadContent = &gmproto.MessagePayloadContent{
			MessageContent: &gmproto.MessageContent{Content: text},
		}
	}
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()
	resp, err := cli.SendMessage(ctx, payload)
	if err != nil {
		return Response{OK: false, Error: err.Error()}
	}
	if resp.GetStatus() != gmproto.SendMessageResponse_SUCCESS && resp.GetStatus() != gmproto.SendMessageResponse_UNKNOWN {
		return Response{OK: false, Error: fmt.Sprintf("send status %v", resp.GetStatus())}
	}
	optimistic := &Msg{
		ID:          tmpID,
		ChatID:      req.ChatID,
		Text:        text,
		FromMe:      true,
		Timestamp:   time.Now().UnixMilli(),
		TmpID:       tmpID,
		Status:      "sending",
		Attachments: atts,
	}
	c.mu.Lock()
	c.msgs[req.ChatID] = append(c.msgs[req.ChatID], optimistic)
	c.mu.Unlock()
	c.emit(Event{Event: "message.upsert", Message: optimistic})
	return Response{OK: true, Result: map[string]any{"tmp_id": tmpID}}
}

func mimeFromName(name string) string {
	ext := strings.ToLower(filepath.Ext(name))
	switch ext {
	case ".jpg", ".jpeg":
		return "image/jpeg"
	case ".png":
		return "image/png"
	case ".gif":
		return "image/gif"
	case ".webp":
		return "image/webp"
	case ".mp4":
		return "video/mp4"
	case ".pdf":
		return "application/pdf"
	case ".txt":
		return "text/plain"
	default:
		return "application/octet-stream"
	}
}

func (c *Core) mediaDownload(req Request) Response {
	if req.MediaID == "" || req.KeyB64 == "" {
		return Response{OK: false, Error: "media_id and key_b64 required"}
	}
	key, err := base64.StdEncoding.DecodeString(req.KeyB64)
	if err != nil {
		return Response{OK: false, Error: "bad key_b64"}
	}
	// Cache, not /tmp: downloads persist across reboots and don't fill the tmpfs.
	// Prefix the media id's hash so two attachments both called "image.jpg" don't
	// overwrite each other; an existing file is reused instead of downloaded again.
	name := req.Name
	if name == "" {
		name = req.MediaID
	}
	sum := sha256.Sum256([]byte(req.MediaID))
	path := filepath.Join(mediaCacheDir(), hex.EncodeToString(sum[:6])+"-"+filepath.Base(name))
	if st, err := os.Stat(path); err == nil && st.Size() > 0 {
		return Response{OK: true, Result: map[string]any{"path": path}}
	}
	c.mu.Lock()
	cli := c.client
	c.mu.Unlock()
	if cli == nil {
		return Response{OK: false, Error: "not connected"}
	}
	data, err := cli.DownloadMedia(req.MediaID, key)
	if err != nil {
		return Response{OK: false, Error: err.Error()}
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return Response{OK: false, Error: err.Error()}
	}
	// Write then rename, so a half-written file is never served as cached.
	tmp := path + ".part"
	if err := os.WriteFile(tmp, data, 0o600); err != nil {
		return Response{OK: false, Error: err.Error()}
	}
	if err := os.Rename(tmp, path); err != nil {
		return Response{OK: false, Error: err.Error()}
	}
	return Response{OK: true, Result: map[string]any{"path": path}}
}

func (c *Core) typingSet(req Request) Response {
	if req.ChatID == "" {
		return Response{OK: false, Error: "chat_id required"}
	}
	c.mu.Lock()
	cli := c.client
	c.mu.Unlock()
	if cli == nil {
		return Response{OK: true}
	}
	if !req.Typing {
		return Response{OK: true}
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := cli.SetTyping(ctx, req.ChatID, nil); err != nil {
		return Response{OK: false, Error: err.Error()}
	}
	return Response{OK: true}
}

func (c *Core) messagesReact(req Request) Response {
	if req.ChatID == "" || req.MsgID == "" || req.Emoji == "" {
		return Response{OK: false, Error: "chat_id, message_id and emoji required"}
	}
	c.mu.Lock()
	cli := c.client
	c.mu.Unlock()
	if cli == nil {
		return Response{OK: false, Error: "not connected"}
	}
	action := gmproto.SendReactionRequest_ADD
	if req.Remove {
		action = gmproto.SendReactionRequest_REMOVE
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	_, err := cli.SendReaction(ctx, &gmproto.SendReactionRequest{
		MessageID:    req.MsgID,
		ReactionData: gmproto.MakeReactionData(req.Emoji),
		Action:       action,
	})
	if err != nil {
		return Response{OK: false, Error: err.Error()}
	}
	return Response{OK: true}
}

func (c *Core) messagesMarkRead(req Request) Response {
	if req.ChatID == "" || req.MsgID == "" {
		return Response{OK: false, Error: "chat_id and message_id required"}
	}
	c.mu.Lock()
	cli := c.client
	if chat, ok := c.chats[req.ChatID]; ok {
		chat.Unread = false
	}
	c.mu.Unlock()
	if cli == nil {
		return Response{OK: true}
	}
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	if err := cli.MarkRead(ctx, req.ChatID, req.MsgID); err != nil {
		return Response{OK: false, Error: err.Error()}
	}
	return Response{OK: true}
}

func cloneMap(in map[string]string) map[string]string {
	out := make(map[string]string, len(in))
	for k, v := range in {
		out[k] = v
	}
	return out
}

// mediaCacheDir is $XDG_CACHE_HOME/nyx-messages/media (~/.cache/... by default).
func mediaCacheDir() string {
	base := os.Getenv("XDG_CACHE_HOME")
	if base == "" {
		if home, err := os.UserHomeDir(); err == nil {
			base = filepath.Join(home, ".cache")
		} else {
			base = os.TempDir()
		}
	}
	return filepath.Join(base, "nyx-messages", "media")
}
