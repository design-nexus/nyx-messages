// nyx-messagesd — Google Messages session daemon (AGPL-3.0).
// Wraps mautrix libgm; the GTK app talks to it over a private Unix socket.
package main

import (
	"os"
	"os/signal"
	"path/filepath"
	"syscall"

	"github.com/rs/zerolog"
)

func main() {
	zerolog.TimeFieldFormat = zerolog.TimeFormatUnixMs
	log := zerolog.New(os.Stderr).With().Timestamp().Str("app", "nyx-messagesd").Logger()

	runtimeDir := os.Getenv("XDG_RUNTIME_DIR")
	if runtimeDir == "" {
		log.Fatal().Msg("XDG_RUNTIME_DIR is required")
	}
	dataDir := os.Getenv("XDG_DATA_HOME")
	if dataDir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			log.Fatal().Err(err).Msg("home")
		}
		dataDir = filepath.Join(home, ".local", "share")
	}
	paths := Paths{
		Socket:  filepath.Join(runtimeDir, "nyx-messages", "bus"),
		Session: filepath.Join(dataDir, "nyx-messages", "session.json"),
	}

	core := NewCore(paths, log)
	if err := core.Boot(); err != nil {
		log.Fatal().Err(err).Msg("boot")
	}

	bus, err := Listen(paths.Socket, core, log)
	if err != nil {
		log.Fatal().Err(err).Msg("listen")
	}
	log.Info().Str("socket", paths.Socket).Msg("listening")

	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGINT, syscall.SIGTERM)
	<-sig
	log.Info().Msg("shutting down")
	bus.Close()
	core.Shutdown()
}
