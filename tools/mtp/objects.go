package main

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path"
	"path/filepath"
	"sort"
	"strings"

	"tyranny-mtp/internal/mtp"
)

func (s *session) storages() error {
	var ids mtp.Uint32Array
	if err := s.usb.GetStorageIDs(&ids); err != nil {
		return fmt.Errorf("list storage: %w", err)
	}
	storages := make([]Storage, 0, len(ids.Values))
	for _, id := range ids.Values {
		var info mtp.StorageInfo
		if err := s.usb.GetStorageInfo(id, &info); err != nil {
			return err
		}
		name := info.StorageDescription
		if name == "" {
			name = info.VolumeLabel
		}
		if name == "" {
			name = fmt.Sprintf("Storage %08x", id)
		}
		storages = append(storages, Storage{id, name, info.MaxCapability, info.FreeSpaceInBytes})
	}
	s.device.Storages = storages
	return nil
}

func audioFormat(name string) (uint16, bool) {
	// macOS resource-fork sidecars keep the audio extension but are not songs.
	if strings.HasPrefix(path.Base(name), "._") {
		return 0, false
	}
	switch strings.ToLower(path.Ext(name)) {
	case ".mp3":
		return mtp.OFC_MP3, true
	case ".wav", ".wave":
		return mtp.OFC_WAV, true
	case ".aif", ".aiff":
		return mtp.OFC_AIFF, true
	case ".flac":
		return mtp.OFC_MTP_FLAC, true
	case ".ogg", ".oga", ".opus":
		return mtp.OFC_MTP_OGG, true
	case ".aac", ".m4a", ".m4b":
		return mtp.OFC_MTP_AAC, true
	case ".wma":
		return mtp.OFC_MTP_WMA, true
	case ".alac", ".ape", ".wv", ".dsf", ".dff", ".dsd", ".mp2", ".mod", ".xm", ".it", ".s3m":
		return mtp.OFC_MTP_UndefinedAudio, true
	default:
		return 0, false
	}
}

func isAudio(info mtp.ObjectInfo) bool {
	if info.ObjectFormat == mtp.OFC_Association || strings.HasSuffix(info.Filename, ".tyranny-partial") {
		return false
	}
	_, ok := audioFormat(info.Filename)
	return ok
}

func (s *session) inventory() error {
	if err := s.storages(); err != nil {
		return err
	}
	objects := map[uint32]mtp.ObjectInfo{}
	for _, storage := range s.device.Storages {
		// Android's all-objects query can stall while recursively indexing a large
		// card. Walk bounded directory listings, as OpenMTP does, instead.
		parents := []uint32{0xffffffff}
		for len(parents) > 0 {
			parent := parents[len(parents)-1]
			parents = parents[:len(parents)-1]
			var handles mtp.Uint32Array
			if err := s.usb.GetObjectHandles(storage.ID, 0, parent, &handles); err != nil {
				return fmt.Errorf("inventory %s directory %d: %w", storage.Name, parent, err)
			}
			for _, handle := range handles.Values {
				var info mtp.ObjectInfo
				if err := s.usb.GetObjectInfo(handle, &info); err != nil {
					return fmt.Errorf("object %d: %w", handle, err)
				}
				rootParent := parent == 0xffffffff && (info.ParentObject == 0 || info.ParentObject == 0xffffffff)
				if info.StorageID != storage.ID || (!rootParent && info.ParentObject != parent) {
					return errors.New("device returned inconsistent object storage or parent")
				}
				if _, exists := objects[handle]; exists {
					return errors.New("device returned ambiguous object handles")
				}
				objects[handle] = info
				if info.ObjectFormat == mtp.OFC_Association {
					parents = append(parents, handle)
				}
			}
		}
	}
	paths := map[uint32]string{}
	var resolve func(uint32, map[uint32]bool) (string, error)
	resolve = func(id uint32, visiting map[uint32]bool) (string, error) {
		if p, exists := paths[id]; exists {
			return p, nil
		}
		info, exists := objects[id]
		if !exists {
			return "", fmt.Errorf("object %d has a missing parent", id)
		}
		if visiting[id] {
			return "", errors.New("cyclic device directory tree")
		}
		if info.Filename == "" || info.Filename == "." || info.Filename == ".." || strings.ContainsAny(info.Filename, "/\\\x00") {
			return "", errors.New("unsafe device object filename")
		}
		visiting[id] = true
		parent := "/"
		if info.ParentObject != 0 && info.ParentObject != 0xffffffff {
			pinfo, exists := objects[info.ParentObject]
			if !exists || pinfo.StorageID != info.StorageID || pinfo.ObjectFormat != mtp.OFC_Association {
				return "", errors.New("invalid device parent directory")
			}
			var err error
			parent, err = resolve(info.ParentObject, visiting)
			if err != nil {
				return "", err
			}
		}
		delete(visiting, id)
		paths[id] = path.Join(parent, info.Filename)
		return paths[id], nil
	}
	songs := []Song{}
	for id, info := range objects {
		p, err := resolve(id, map[uint32]bool{})
		if err != nil {
			return err
		}
		if !isAudio(info) {
			continue
		}
		size := uint64(info.CompressedSize)
		if info.CompressedSize == 0xffffffff {
			var value mtp.Uint64Value
			if err := s.usb.GetObjectPropValue(id, mtp.OPC_ObjectSize, &value); err != nil {
				return err
			}
			size = value.Value
		}
		songs = append(songs, Song{ID: id, StorageID: info.StorageID, Path: p, Name: info.Filename, Size: size})
	}
	sort.Slice(songs, func(i, j int) bool {
		if songs[i].StorageID != songs[j].StorageID {
			return songs[i].StorageID < songs[j].StorageID
		}
		return songs[i].Path < songs[j].Path
	})
	s.objects, s.paths, s.device.Songs = objects, paths, songs
	return nil
}

func (s *session) revalidate(id uint32) (mtp.ObjectInfo, error) {
	before, exists := s.objects[id]
	if !exists {
		return mtp.ObjectInfo{}, errors.New("object is no longer in inventory")
	}
	var current mtp.ObjectInfo
	if err := s.usb.GetObjectInfo(id, &current); err != nil {
		return current, err
	}
	if before.ObjectFormat == mtp.OFC_Association {
		// Directory dates describe changes to children, including our own staging
		// write. They are not identity. Keep full metadata equality for audio files.
		before.CaptureDate = current.CaptureDate
		before.ModificationDate = current.ModificationDate
	}
	if current != before {
		return current, errors.New("device object identity changed; refresh before trying again")
	}
	deviceID, _, err := identity(s.usb)
	if err != nil {
		return current, err
	}
	if deviceID != s.device.ID {
		return current, errors.New("USB device identity changed")
	}
	return current, nil
}

var errIncompleteObject = errors.New("incomplete object content")

type countWriter struct {
	writer io.Writer
	count  uint64
}

func (w *countWriter) Write(p []byte) (int, error) {
	n, err := w.writer.Write(p)
	w.count += uint64(n)
	return n, err
}

func (s *session) objectDigest(id uint32, size uint64) (string, error) {
	if _, err := s.revalidate(id); err != nil {
		return "", err
	}
	hash := sha256.New()
	counter := &countWriter{writer: hash}
	if err := s.usb.GetObject(id, counter, mtp.EmptyProgressFunc); err != nil {
		return "", fmt.Errorf("read object %d: %w", id, err)
	}
	if counter.count != size {
		return "", fmt.Errorf("%w: object %d received %d of %d bytes", errIncompleteObject, id, counter.count, size)
	}
	if _, err := s.revalidate(id); err != nil {
		return "", err
	}
	return hex.EncodeToString(hash.Sum(nil)), nil
}

func localDigest(p string) (string, uint64, error) {
	if !filepath.IsAbs(p) {
		return "", 0, errors.New("local path must be absolute")
	}
	if _, audio := audioFormat(p); !audio {
		return "", 0, fmt.Errorf("unsupported audio extension: %s", p)
	}
	file, err := os.Open(p)
	if err != nil {
		return "", 0, err
	}
	defer file.Close()
	before, err := file.Stat()
	if err != nil {
		return "", 0, err
	}
	if !before.Mode().IsRegular() {
		return "", 0, errors.New("source must be a regular file")
	}
	hash := sha256.New()
	n, err := io.Copy(hash, file)
	if err != nil {
		return "", 0, err
	}
	after, err := file.Stat()
	if err != nil {
		return "", 0, err
	}
	if n != before.Size() || after.Size() != before.Size() || after.ModTime() != before.ModTime() {
		return "", 0, errors.New("source changed while hashing")
	}
	return hex.EncodeToString(hash.Sum(nil)), uint64(n), nil
}

func (s *session) findMatch(digest string, size uint64) (*Song, error) {
	for i := range s.device.Songs {
		song := &s.device.Songs[i]
		if song.Size != size {
			continue
		}
		if song.SHA256 == "" {
			value, err := s.objectDigest(song.ID, song.Size)
			if err != nil {
				return nil, err
			}
			song.SHA256 = value
		}
		if song.SHA256 == digest {
			if _, err := s.revalidate(song.ID); err != nil {
				return nil, err
			}
			return song, nil
		}
	}
	return nil, nil
}

func (s *session) matchFiles(files []LocalFile, response *Response) error {
	for _, file := range files {
		digest, size, err := localDigest(file.Path)
		if err != nil {
			return fmt.Errorf("%s: %w", file.Path, err)
		}
		song, err := s.findMatch(digest, size)
		if err != nil {
			return err
		}
		if song != nil {
			response.Matches = append(response.Matches, Match{file.Path, song.ID, song.StorageID, digest})
		}
	}
	return nil
}

func (s *session) writable(storageID uint32, deleting bool) (mtp.StorageInfo, error) {
	deviceID, _, err := identity(s.usb)
	if err != nil {
		return mtp.StorageInfo{}, err
	}
	if deviceID != s.device.ID {
		return mtp.StorageInfo{}, errors.New("USB identity changed")
	}
	found := false
	for _, storage := range s.device.Storages {
		if storage.ID == storageID {
			found = true
		}
	}
	if !found {
		return mtp.StorageInfo{}, errors.New("selected storage disconnected")
	}
	var info mtp.StorageInfo
	if err := s.usb.GetStorageInfo(storageID, &info); err != nil {
		return info, err
	}
	if info.AccessCapability != mtp.AC_ReadWrite && !(deleting && info.AccessCapability == mtp.AC_ReadOnly_with_Object_Deletion) {
		return info, errors.New("selected storage is read-only")
	}
	return info, nil
}

func (s *session) deleteSongs(req Request, response *Response) error {
	expected := map[uint32]Song{}
	for _, song := range req.ExpectedSongs {
		expected[song.ID] = song
	}
	currentSizes := map[uint32]uint64{}
	for _, song := range s.device.Songs {
		currentSizes[song.ID] = song.Size
	}
	seen := map[uint32]bool{}
	// Validate the entire explicit selection before deleting the first object.
	for _, id := range req.ObjectIDs {
		if seen[id] {
			return errors.New("duplicate deletion object ID")
		}
		seen[id] = true
		info, err := s.revalidate(id)
		if err != nil {
			return err
		}
		want, ok := expected[id]
		if !ok || want.StorageID != req.StorageID || info.StorageID != req.StorageID || !isAudio(info) || want.Path != s.paths[id] || want.Name != info.Filename || want.Size != currentSizes[id] {
			return errors.New("deletion selection changed or is not an audio file in the selected storage; refresh first")
		}
		if want.SHA256 != "" {
			actual, err := s.objectDigest(id, want.Size)
			if err != nil {
				return err
			}
			if actual != want.SHA256 {
				return errors.New("deletion content changed; refresh first")
			}
		}
	}
	for _, id := range req.ObjectIDs {
		if _, err := s.writable(req.StorageID, true); err != nil {
			return err
		}
		if _, err := s.revalidate(id); err != nil {
			return err
		}
		if err := s.usb.DeleteObject(id); err != nil {
			return fmt.Errorf("delete object %d: %w", id, err)
		}
	}
	return nil
}
