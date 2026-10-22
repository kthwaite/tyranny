// tyranny-likes-smoke-mtp is a single-request Sony Walkman MTP process. USB diagnostics are
// isolated from its JSON channel, including output from the native dependencies.
package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"syscall"

	"github.com/ganeshrvel/usb"
	"tyranny-likes-smoke-mtp/internal/mtp"
)

type Request struct {
	Operation     string      `json:"operation"`
	DeviceID      string      `json:"device_id"`
	StorageID     uint32      `json:"storage_id"`
	Files         []LocalFile `json:"files"`
	ObjectIDs     []uint32    `json:"object_ids"`
	ExpectedSongs []Song      `json:"expected_songs"`
}
type LocalFile struct {
	Path string `json:"path"`
}
type Storage struct {
	ID       uint32 `json:"id"`
	Name     string `json:"name"`
	Capacity uint64 `json:"capacity"`
	Free     uint64 `json:"free"`
}
type Song struct {
	ID        uint32 `json:"id"`
	StorageID uint32 `json:"storage_id"`
	Path      string `json:"path"`
	Name      string `json:"name"`
	Size      uint64 `json:"size"`
	SHA256    string `json:"sha256,omitempty"`
}
type Device struct {
	ID       string    `json:"id"`
	Name     string    `json:"name"`
	Storages []Storage `json:"storages"`
	Songs    []Song    `json:"songs"`
}
type Match struct {
	Path      string `json:"path"`
	ObjectID  uint32 `json:"object_id"`
	StorageID uint32 `json:"storage_id"`
	SHA256    string `json:"sha256"`
}
type Response struct {
	Devices []Device `json:"devices,omitempty"`
	Device  *Device  `json:"device,omitempty"`
	Copied  int      `json:"copied"`
	Skipped int      `json:"skipped"`
	Matches []Match  `json:"matches"`
	Error   string   `json:"error,omitempty"`
}

// The seam is the upstream MTP object transport, not the sync algorithm. Tests
// exercise the same inventory, hashing, journal, recovery and mutation engine.
type objectTransport interface {
	GetUsbInfo() (*mtp.UsbDeviceInfo, error)
	GetStorageIDs(*mtp.Uint32Array) error
	GetStorageInfo(uint32, *mtp.StorageInfo) error
	GetObjectHandles(uint32, uint32, uint32, *mtp.Uint32Array) error
	GetObjectInfo(uint32, *mtp.ObjectInfo) error
	GetObjectPropValue(uint32, uint16, interface{}) error
	SetObjectPropValue(uint32, uint16, interface{}) error
	GetObject(uint32, io.Writer, mtp.ProgressFunc) error
	DeleteObject(uint32) error
	SendObjectInfo(uint32, uint32, *mtp.ObjectInfo) (uint32, uint32, uint32, error)
	SendObject(io.Reader, int64, mtp.ProgressFunc) error
}

type session struct {
	usb      objectTransport
	device   Device
	objects  map[uint32]mtp.ObjectInfo
	paths    map[uint32]string
	stateDir string
}

func main() {
	// Save the protocol channel, then redirect fd 1 itself (not just os.Stdout)
	// so C and upstream fmt.Printf cannot corrupt the response.
	fd, err := syscall.Dup(1)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	out := os.NewFile(uintptr(fd), "json-response")
	defer out.Close()
	if err := syscall.Dup2(2, 1); err != nil {
		_ = json.NewEncoder(out).Encode(Response{Error: err.Error()})
		return
	}
	response := Response{Matches: []Match{}}
	defer func() {
		if p := recover(); p != nil {
			response.Error = fmt.Sprintf("MTP helper failed: %v", p)
		}
		if err := json.NewEncoder(out).Encode(response); err != nil {
			fmt.Fprintln(os.Stderr, err)
		}
	}()
	decoder := json.NewDecoder(io.LimitReader(os.Stdin, 16<<20))
	decoder.DisallowUnknownFields()
	var request Request
	if err := decoder.Decode(&request); err != nil {
		response.Error = "Invalid request: " + err.Error()
		return
	}
	var extra any
	if err := decoder.Decode(&extra); err != io.EOF {
		response.Error = "Expected one JSON request"
		return
	}
	if err := run(request, &response); err != nil {
		response.Error = err.Error() + ". Unlock the Walkman, select USB File Transfer, and close OpenMTP or other MTP applications if busy."
	}
}

func stateDirectory() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	// Preserve the installed state path so existing upload journals and locks remain shared.
	dir := filepath.Join(home, "Library", "Application Support", "tyranny", "mtp")
	if err := os.MkdirAll(dir, 0700); err != nil {
		return "", err
	}
	return dir, nil
}

func identity(d objectTransport) (string, string, error) {
	info, err := d.GetUsbInfo()
	if err != nil {
		return "", "", err
	}
	if info.IdVendor != 0x054c || strings.TrimSpace(info.SerialNumber) == "" {
		return "", "", errors.New("unsupported device or missing stable USB serial")
	}
	id := fmt.Sprintf("usb:%04x:%04x:%s", info.IdVendor, info.IdProduct, hex.EncodeToString([]byte(info.SerialNumber)))
	return id, strings.TrimSpace(info.Manufacturer + " " + info.Product), nil
}

func run(req Request, response *Response) error {
	switch req.Operation {
	case "discover", "inventory", "match", "sync", "delete":
	default:
		return errors.New("unknown operation")
	}
	if req.Operation != "discover" && req.DeviceID == "" {
		return errors.New("device_id is required")
	}
	if (req.Operation == "sync" || req.Operation == "delete") && req.StorageID == 0 {
		return errors.New("storage_id is required")
	}
	dir, err := stateDirectory()
	if err != nil {
		return err
	}
	lock, err := os.OpenFile(filepath.Join(dir, "session.lock"), os.O_CREATE|os.O_RDWR, 0600)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		return errors.New("another tyranny likes smoke MTP operation is busy")
	}
	defer syscall.Flock(int(lock.Fd()), syscall.LOCK_UN)
	context := usb.NewContext()
	if context == nil {
		return errors.New("cannot initialize USB")
	}
	defer context.Exit()
	candidates, err := mtp.FindDevices(context)
	if err != nil {
		return err
	}
	defer func() {
		for _, d := range candidates {
			d.Close()
			d.Done()
		}
	}()
	var selected *session
	var discoveryErrors []string
	seen := map[string]bool{}
	for _, d := range candidates {
		d.Timeout = 5000
		if err := d.Open(); err != nil {
			discoveryErrors = append(discoveryErrors, err.Error())
			continue
		}
		id, name, err := identity(d)
		if err != nil {
			discoveryErrors = append(discoveryErrors, err.Error())
			d.Close()
			continue
		}
		if seen[id] {
			return errors.New("ambiguous devices share the same USB identity; disconnect one")
		}
		seen[id] = true
		if req.Operation != "discover" && id != req.DeviceID {
			d.Close()
			continue
		}
		// OpenSession, not Configure: never reset another application's session.
		if err := d.OpenSession(); err != nil {
			discoveryErrors = append(discoveryErrors, err.Error())
			d.Close()
			continue
		}
		s := &session{usb: d, device: Device{ID: id, Name: name, Storages: []Storage{}, Songs: []Song{}}, stateDir: dir}
		if err := s.storages(); err != nil {
			discoveryErrors = append(discoveryErrors, err.Error())
			d.Close()
			continue
		}
		if req.Operation == "discover" {
			response.Devices = append(response.Devices, s.device)
			d.Close()
		} else {
			// Android may need substantially longer to enumerate a large SD card
			// than to answer the small storage query used by discovery.
			d.Timeout = 60000
			selected = s
		}
	}
	if req.Operation == "discover" {
		if len(discoveryErrors) != 0 {
			return errors.New(strings.Join(discoveryErrors, "; "))
		}
		return nil
	}
	if selected == nil {
		if len(discoveryErrors) != 0 {
			return errors.New(strings.Join(discoveryErrors, "; "))
		}
		return errors.New("selected device disconnected or no longer in MTP mode")
	}
	s := selected
	if err := s.inventory(); err != nil {
		return err
	}
	response.Device = &s.device
	if req.Operation == "inventory" {
		return nil
	}
	var operationErr error
	switch req.Operation {
	case "match":
		operationErr = s.matchFiles(req.Files, response)
	case "sync":
		operationErr = s.syncFiles(req.StorageID, req.Files, response)
	case "delete":
		operationErr = s.deleteSongs(req, response)
	}
	if req.Operation == "sync" || req.Operation == "delete" {
		if err := s.inventory(); err != nil {
			response.Device = nil // Never represent stale free space or objects as refreshed.
			operationErr = errors.Join(operationErr, fmt.Errorf("refresh after operation: %w", err))
		}
	}
	return operationErr
}

func digestString(value string) string {
	sum := sha256.Sum256([]byte(value))
	return hex.EncodeToString(sum[:])
}
