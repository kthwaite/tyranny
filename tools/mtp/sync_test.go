package main

import (
	"errors"
	"io"
	"os"
	"path/filepath"
	"testing"
	"time"

	"tyranny-likes-smoke-mtp/internal/mtp"
)

// An in-memory object transport, not a fake sync implementation. It deliberately
// retains incomplete MTP objects, supports handle reuse, and counts mutations.
type memoryTransport struct {
	objects    map[uint32]mtp.ObjectInfo
	content    map[uint32][]byte
	free       uint64
	next       uint32
	pending    uint32
	writes     int
	uploads    int
	interrupt  bool
	failRename bool
}

func newTransport() *memoryTransport {
	return &memoryTransport{objects: map[uint32]mtp.ObjectInfo{}, content: map[uint32][]byte{}, free: 1 << 30, next: 10}
}
func (m *memoryTransport) GetUsbInfo() (*mtp.UsbDeviceInfo, error) {
	return &mtp.UsbDeviceInfo{IdVendor: 0x054c, IdProduct: 0x0e6e, SerialNumber: "unit-test", Product: "WALKMAN"}, nil
}
func (m *memoryTransport) GetStorageIDs(out *mtp.Uint32Array) error {
	out.Values = []uint32{1, 2}
	return nil
}
func (m *memoryTransport) GetStorageInfo(id uint32, out *mtp.StorageInfo) error {
	if id != 1 && id != 2 {
		return errors.New("missing storage")
	}
	*out = mtp.StorageInfo{AccessCapability: mtp.AC_ReadWrite, MaxCapability: 1 << 30, FreeSpaceInBytes: m.free, StorageDescription: "test"}
	return nil
}
func (m *memoryTransport) GetObjectHandles(storage, format, parent uint32, out *mtp.Uint32Array) error {
	if parent == 0 {
		return errors.New("all-objects enumeration is not supported by this transport")
	}
	for id, info := range m.objects {
		root := parent == 0xffffffff && (info.ParentObject == 0 || info.ParentObject == 0xffffffff)
		if info.StorageID == storage && (root || info.ParentObject == parent) {
			out.Values = append(out.Values, id)
		}
	}
	return nil
}
func (m *memoryTransport) GetObjectInfo(id uint32, out *mtp.ObjectInfo) error {
	info, ok := m.objects[id]
	if !ok {
		return errors.New("missing object")
	}
	*out = info
	return nil
}
func (m *memoryTransport) GetObjectPropValue(id uint32, prop uint16, out interface{}) error {
	return errors.New("unexpected property read")
}
func (m *memoryTransport) SetObjectPropValue(id uint32, prop uint16, value interface{}) error {
	if m.failRename {
		m.failRename = false
		return errors.New("disconnected before rename")
	}
	if prop != mtp.OPC_ObjectFileName {
		return errors.New("unexpected property write")
	}
	info := m.objects[id]
	info.Filename = value.(*mtp.StringValue).Value
	m.objects[id] = info
	m.touchDirectory(info.ParentObject)
	m.writes++
	return nil
}
func (m *memoryTransport) GetObject(id uint32, w io.Writer, cb mtp.ProgressFunc) error {
	_, err := w.Write(m.content[id])
	return err
}
func (m *memoryTransport) DeleteObject(id uint32) error {
	parent := m.objects[id].ParentObject
	delete(m.objects, id)
	delete(m.content, id)
	m.touchDirectory(parent)
	m.writes++
	return nil
}
func (m *memoryTransport) SendObjectInfo(storage, parent uint32, info *mtp.ObjectInfo) (uint32, uint32, uint32, error) {
	m.next++
	id := m.next
	m.objects[id] = *info
	m.touchDirectory(parent)
	m.pending = id
	m.writes++
	return storage, parent, id, nil
}

// Android updates directory timestamps when children are created, renamed or removed.
func (m *memoryTransport) touchDirectory(id uint32) {
	info, exists := m.objects[id]
	if !exists {
		return
	}
	info.CaptureDate = info.CaptureDate.Add(time.Second)
	info.ModificationDate = info.ModificationDate.Add(time.Second)
	m.objects[id] = info
}

func (m *memoryTransport) SendObject(r io.Reader, size int64, cb mtp.ProgressFunc) error {
	m.uploads++
	data, err := io.ReadAll(r)
	if err != nil {
		return err
	}
	if m.interrupt {
		m.interrupt = false
		m.content[m.pending] = data[:len(data)/2]
		return io.ErrUnexpectedEOF
	}
	m.content[m.pending] = data
	return nil
}
func (m *memoryTransport) add(storage uint32, name string, data []byte) uint32 {
	m.next++
	id := m.next
	m.objects[id] = mtp.ObjectInfo{StorageID: storage, Filename: name, CompressedSize: uint32(len(data)), ObjectFormat: mtp.OFC_MP3}
	m.content[id] = data
	return id
}
func testSession(t *testing.T, m *memoryTransport, dir string) *session {
	t.Helper()
	id, name, err := identity(m)
	if err != nil {
		t.Fatal(err)
	}
	s := &session{usb: m, device: Device{ID: id, Name: name}, stateDir: dir}
	if err := s.inventory(); err != nil {
		t.Fatal(err)
	}
	return s
}
func testFile(t *testing.T, name, content string) LocalFile {
	t.Helper()
	p := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(p, []byte(content), 0600); err != nil {
		t.Fatal(err)
	}
	return LocalFile{p}
}

func TestContentIdentityAcrossNamesAndStorages(t *testing.T) {
	m := newTransport()
	id := m.add(1, "OpenMTP title.mp3", []byte("same audio"))
	s := testSession(t, m, t.TempDir())
	local := testFile(t, "renamed.mp3", "same audio")
	var response Response
	if err := s.syncFiles(2, []LocalFile{local}, &response); err != nil {
		t.Fatal(err)
	}
	if response.Copied != 0 || response.Skipped != 1 || len(response.Matches) != 1 || response.Matches[0].ObjectID != id || response.Matches[0].StorageID != 1 || m.writes != 0 {
		t.Fatalf("not content-idempotent: %+v writes=%d", response, m.writes)
	}
}
func TestSameTitleDifferentBytesUploads(t *testing.T) {
	m := newTransport()
	original := m.add(1, "song.mp3", []byte("old audio"))
	s := testSession(t, m, t.TempDir())
	local := testFile(t, "song.mp3", "new audio")
	var response Response
	if err := s.syncFiles(1, []LocalFile{local}, &response); err != nil {
		t.Fatal(err)
	}
	if response.Copied != 1 || response.Skipped != 0 || m.uploads != 1 || string(m.content[original]) != "old audio" {
		t.Fatalf("wrong upload result: %+v", response)
	}
}
func TestInterruptedUploadRecoversWithoutDuplicate(t *testing.T) {
	m := newTransport()
	m.interrupt = true
	dir := t.TempDir()
	local := testFile(t, "song.mp3", "complete audio content")
	s := testSession(t, m, dir)
	var first Response
	if err := s.syncFiles(1, []LocalFile{local}, &first); err == nil {
		t.Fatal("interrupted upload succeeded")
	}
	if first.Copied != 0 || len(first.Matches) != 0 {
		t.Fatalf("partial upload reported synced: %+v", first)
	}
	// Fresh engine instance models helper/process restart with the durable journal.
	s = testSession(t, m, dir)
	var retry Response
	if err := s.syncFiles(1, []LocalFile{local}, &retry); err != nil {
		t.Fatal(err)
	}
	if err := s.inventory(); err != nil {
		t.Fatal(err)
	}
	if retry.Copied != 1 || len(s.device.Songs) != 1 {
		t.Fatalf("retry duplicated or lost content: %+v songs=%v", retry, s.device.Songs)
	}
	var again Response
	if err := s.syncFiles(1, []LocalFile{local}, &again); err != nil {
		t.Fatal(err)
	}
	if again.Skipped != 1 || m.uploads != 2 {
		t.Fatalf("second retry copied again: %+v uploads=%d", again, m.uploads)
	}
}
func TestInterruptedCommitRecoversWithoutRecopy(t *testing.T) {
	m := newTransport()
	m.failRename = true
	dir := t.TempDir()
	local := testFile(t, "song.mp3", "complete audio")
	s := testSession(t, m, dir)
	var first Response
	if err := s.syncFiles(1, []LocalFile{local}, &first); err == nil || first.Copied != 0 {
		t.Fatal("failed commit counted as synced")
	}
	s = testSession(t, m, dir)
	var retry Response
	if err := s.syncFiles(1, []LocalFile{local}, &retry); err != nil {
		t.Fatal(err)
	}
	if retry.Skipped != 1 || m.uploads != 1 || len(s.device.Songs) != 1 {
		t.Fatalf("committed bytes recopied: %+v uploads=%d", retry, m.uploads)
	}
}
func TestStaleDeletionAndFoldersAreRejected(t *testing.T) {
	m := newTransport()
	id := m.add(1, "original.mp3", []byte("audio"))
	s := testSession(t, m, t.TempDir())
	snapshot := s.device.Songs[0]
	info := m.objects[id]
	info.Filename = "replacement.mp3"
	m.objects[id] = info
	s = testSession(t, m, s.stateDir)
	err := s.deleteSongs(Request{StorageID: 1, ObjectIDs: []uint32{id}, ExpectedSongs: []Song{snapshot}}, &Response{})
	if err == nil || m.writes != 0 {
		t.Fatal("stale handle deletion accepted")
	}
	info.ObjectFormat = mtp.OFC_Association
	m.objects[id] = info
	s = testSession(t, m, s.stateDir)
	snapshot.Name = info.Filename
	snapshot.Path = "/" + info.Filename
	if err := s.deleteSongs(Request{StorageID: 1, ObjectIDs: []uint32{id}, ExpectedSongs: []Song{snapshot}}, &Response{}); err == nil || m.writes != 0 {
		t.Fatal("folder deletion accepted")
	}
}
func TestInsufficientSpaceDoesNotBeginCopy(t *testing.T) {
	m := newTransport()
	m.free = 1
	s := testSession(t, m, t.TempDir())
	local := testFile(t, "song.mp3", "too much audio")
	if err := s.syncFiles(1, []LocalFile{local}, &Response{}); err == nil {
		t.Fatal("insufficient space accepted")
	}
	if m.writes != 0 || m.uploads != 0 {
		t.Fatal("copy began despite insufficient storage")
	}
}
func TestReadOnlyMatchAndExplicitDelete(t *testing.T) {
	m := newTransport()
	id := m.add(1, "song.mp3", []byte("audio"))
	other := m.add(1, "other.mp3", []byte("other"))
	s := testSession(t, m, t.TempDir())
	local := testFile(t, "renamed.mp3", "audio")
	var matched Response
	if err := s.matchFiles([]LocalFile{local}, &matched); err != nil {
		t.Fatal(err)
	}
	if len(matched.Matches) != 1 || m.writes != 0 {
		t.Fatal("match mutated or missed existing audio")
	}
	var selected Song
	for _, song := range s.device.Songs {
		if song.ID == id {
			selected = song
		}
	}
	if err := s.deleteSongs(Request{StorageID: 1, ObjectIDs: []uint32{id}, ExpectedSongs: []Song{selected}}, &Response{}); err != nil {
		t.Fatal(err)
	}
	if _, ok := m.objects[id]; ok {
		t.Fatal("selected song not removed")
	}
	if _, ok := m.objects[other]; !ok {
		t.Fatal("unselected song removed")
	}
}

func TestInventoryWalksNestedStorageDirectories(t *testing.T) {
	m := newTransport()
	m.objects[4] = mtp.ObjectInfo{StorageID: 2, ParentObject: 0xffffffff, Filename: "Music", ObjectFormat: mtp.OFC_Association}
	m.objects[5] = mtp.ObjectInfo{StorageID: 2, ParentObject: 4, Filename: "Album", ObjectFormat: mtp.OFC_Association}
	id := m.add(2, "song.flac", []byte("nested audio"))
	info := m.objects[id]
	info.ParentObject = 5
	m.objects[id] = info
	metadata := m.add(2, "._song.flac", []byte("AppleDouble metadata"))
	info = m.objects[metadata]
	info.ParentObject = 5
	m.objects[metadata] = info
	s := testSession(t, m, t.TempDir())
	if len(s.device.Songs) != 1 || s.device.Songs[0].Path != "/Music/Album/song.flac" || s.device.Songs[0].StorageID != 2 {
		t.Fatalf("nested inventory lost storage/path: %+v", s.device.Songs)
	}
	if m.writes != 0 {
		t.Fatal("inventory mutated device")
	}
}

func TestChangedManagedDirectoryRefusesFurtherCopies(t *testing.T) {
	m := newTransport()
	s := testSession(t, m, t.TempDir())
	var first Response
	if err := s.syncFiles(1, []LocalFile{testFile(t, "first.mp3", "first audio")}, &first); err != nil {
		t.Fatal(err)
	}
	parent := m.objects[first.Matches[0].ObjectID].ParentObject
	changed := m.objects[parent]
	changed.Filename = "Unrelated"
	m.objects[parent] = changed
	if err := s.syncFiles(1, []LocalFile{testFile(t, "second.mp3", "different audio")}, &Response{}); err == nil {
		t.Fatal("copy accepted a renamed managed directory")
	}
	if m.uploads != 1 {
		t.Fatal("copy wrote into the changed directory")
	}
}
