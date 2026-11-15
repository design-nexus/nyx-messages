package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"go.mau.fi/mautrix-gmessages/pkg/libgm"
)

type Paths struct {
	Socket  string
	Session string
}

func (p Paths) ensureDirs() error {
	for _, d := range []string{filepath.Dir(p.Socket), filepath.Dir(p.Session)} {
		if err := os.MkdirAll(d, 0o700); err != nil {
			return err
		}
	}
	return nil
}

func loadAuth(path string) (*libgm.AuthData, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	var auth libgm.AuthData
	if err := json.NewDecoder(f).Decode(&auth); err != nil {
		return nil, fmt.Errorf("decode session: %w", err)
	}
	return &auth, nil
}

func saveAuth(path string, auth *libgm.AuthData) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(filepath.Dir(path), ".session-*.tmp")
	if err != nil {
		return err
	}
	tmpPath := tmp.Name()
	enc := json.NewEncoder(tmp)
	enc.SetIndent("", "  ")
	if err := enc.Encode(auth); err != nil {
		_ = tmp.Close()
		_ = os.Remove(tmpPath)
		return err
	}
	if err := tmp.Close(); err != nil {
		_ = os.Remove(tmpPath)
		return err
	}
	if err := os.Chmod(tmpPath, 0o600); err != nil {
		_ = os.Remove(tmpPath)
		return err
	}
	return os.Rename(tmpPath, path)
}

func removeAuth(path string) error {
	err := os.Remove(path)
	if err != nil && !os.IsNotExist(err) {
		return err
	}
	return nil
}
