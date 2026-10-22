package main

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"os"
	"path"
	"path/filepath"
	"strings"
	"time"

	"tyranny-likes-smoke-mtp/internal/mtp"
)

// A durable intent precedes SendObjectInfo. Its unpredictable temporary name
// identifies our own orphan even if the process dies before recording the handle.
// No general directory sweep or removal of unjournaled objects is performed.
// Keep /Music/Tyranny and the .tyranny- / .tyranny-partial markers unchanged:
// existing journals must still identify and recover uploads created before the rename.
type pendingUpload struct {
	DeviceID  string `json:"device_id"`
	StorageID uint32 `json:"storage_id"`
	Parent    uint32 `json:"parent"`
	Temporary string `json:"temporary"`
	Final     string `json:"final"`
	Digest    string `json:"digest"`
	Size      uint64 `json:"size"`
}

func (s *session) journalPath(storage uint32) string {
	return filepath.Join(s.stateDir, fmt.Sprintf("pending-%s-%08x.json", digestString(s.device.ID), storage))
}

func durableJSON(filename string, value any) error {
	data, err := json.Marshal(value)
	if err != nil {
		return err
	}
	file, err := os.CreateTemp(filepath.Dir(filename), ".journal-")
	if err != nil {
		return err
	}
	defer os.Remove(file.Name())
	if _, err := file.Write(data); err != nil {
		file.Close()
		return err
	}
	if err := file.Sync(); err != nil {
		file.Close()
		return err
	}
	if err := file.Close(); err != nil {
		return err
	}
	if err := os.Rename(file.Name(), filename); err != nil {
		return err
	}
	dir, err := os.Open(filepath.Dir(filename))
	if err != nil {
		return err
	}
	defer dir.Close()
	return dir.Sync()
}

func (s *session) objectAt(storage uint32, fullPath string) (uint32, bool, error) {
	var found uint32
	for id, p := range s.paths {
		if s.objects[id].StorageID != storage || p != fullPath {
			continue
		}
		if found != 0 {
			return 0, false, fmt.Errorf("ambiguous device path %s", fullPath)
		}
		found = id
	}
	return found, found != 0, nil
}

func (s *session) directory(storage, parent uint32, name, fullPath string) (uint32, error) {
	if id, exists, err := s.objectAt(storage, fullPath); err != nil {
		return 0, err
	} else if exists {
		info, err := s.revalidate(id)
		if err != nil {
			return 0, err
		}
		if info.ObjectFormat != mtp.OFC_Association {
			return 0, fmt.Errorf("%s is not a directory", fullPath)
		}
		return id, nil
	}
	if _, err := s.writable(storage, false); err != nil {
		return 0, err
	}
	if parent != 0xffffffff {
		if _, err := s.revalidate(parent); err != nil {
			return 0, err
		}
	}
	info := mtp.ObjectInfo{StorageID: storage, ParentObject: parent, Filename: name, ObjectFormat: mtp.OFC_Association, AssociationType: mtp.AT_GenericFolder}
	gotStorage, gotParent, id, err := s.usb.SendObjectInfo(storage, parent, &info)
	if err != nil {
		return 0, err
	}
	if gotStorage != storage || gotParent != parent {
		return 0, errors.New("device created directory in unexpected location")
	}
	var actual mtp.ObjectInfo
	if err := s.usb.GetObjectInfo(id, &actual); err != nil {
		return 0, err
	}
	if actual.StorageID != storage || actual.Filename != name || actual.ObjectFormat != mtp.OFC_Association {
		return 0, errors.New("created directory identity mismatch")
	}
	s.objects[id], s.paths[id] = actual, fullPath
	return id, nil
}

func (s *session) managedDirectory(storage uint32) (uint32, error) {
	music, err := s.directory(storage, 0xffffffff, "Music", "/Music")
	if err != nil {
		return 0, err
	}
	return s.directory(storage, music, "Tyranny", "/Music/Tyranny")
}

func (s *session) validatePending(p pendingUpload, storage uint32) error {
	if p.DeviceID != s.device.ID || p.StorageID != storage || len(p.Digest) != 64 || !strings.HasPrefix(p.Temporary, ".tyranny-") || !strings.HasSuffix(p.Temporary, ".tyranny-partial") || path.Base(p.Temporary) != p.Temporary || path.Base(p.Final) != p.Final || !strings.Contains(p.Final, p.Digest) {
		return errors.New("invalid pending upload journal; refusing device cleanup")
	}
	if _, err := hex.DecodeString(p.Digest); err != nil {
		return err
	}
	info, err := s.revalidate(p.Parent)
	if err != nil {
		return err
	}
	if info.StorageID != storage || info.ObjectFormat != mtp.OFC_Association || s.paths[p.Parent] != "/Music/Tyranny" {
		return errors.New("pending upload parent identity changed")
	}
	return nil
}

func (s *session) commitUpload(p pendingUpload, id uint32) error {
	if err := s.validatePending(p, p.StorageID); err != nil {
		return err
	}
	if _, err := s.writable(p.StorageID, false); err != nil {
		return err
	}
	info, err := s.revalidate(id)
	if err != nil {
		return err
	}
	if info.StorageID != p.StorageID || info.ParentObject != p.Parent || info.Filename != p.Temporary || info.ObjectFormat == mtp.OFC_Association {
		return errors.New("staged object identity changed")
	}
	if _, exists, err := s.objectAt(p.StorageID, path.Join("/Music/Tyranny", p.Final)); err != nil {
		return err
	} else if exists {
		return errors.New("destination already exists; refusing to overwrite it")
	}
	if err := s.usb.SetObjectPropValue(id, mtp.OPC_ObjectFileName, &mtp.StringValue{Value: p.Final}); err != nil {
		return fmt.Errorf("commit uploaded filename: %w", err)
	}
	var actual mtp.ObjectInfo
	if err := s.usb.GetObjectInfo(id, &actual); err != nil {
		return err
	}
	if actual.Filename != p.Final || actual.StorageID != p.StorageID || actual.ParentObject != p.Parent || uint64(actual.CompressedSize) != p.Size {
		return errors.New("uploaded object commit could not be confirmed")
	}
	s.objects[id], s.paths[id] = actual, path.Join("/Music/Tyranny", p.Final)
	s.device.Songs = append(s.device.Songs, Song{ID: id, StorageID: p.StorageID, Path: s.paths[id], Name: p.Final, Size: p.Size, SHA256: p.Digest})
	return os.Remove(s.journalPath(p.StorageID))
}

func (s *session) recoverUpload(storage uint32) error {
	data, err := os.ReadFile(s.journalPath(storage))
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	var p pendingUpload
	if err := json.Unmarshal(data, &p); err != nil {
		return err
	}
	if err := s.validatePending(p, storage); err != nil {
		return err
	}
	id, exists, err := s.objectAt(storage, path.Join("/Music/Tyranny", p.Temporary))
	if err != nil {
		return err
	}
	if !exists {
		// No temporary object: either SendObjectInfo never happened or rename
		// already committed. Normal content matching will verify any final file.
		return os.Remove(s.journalPath(storage))
	}
	info, err := s.revalidate(id)
	if err != nil {
		return err
	}
	if info.ParentObject != p.Parent || info.ObjectFormat == mtp.OFC_Association {
		return errors.New("pending object identity changed; refusing cleanup")
	}
	if uint64(info.CompressedSize) == p.Size {
		digest, err := s.objectDigest(id, p.Size)
		if err != nil && !errors.Is(err, errIncompleteObject) && !errors.Is(err, mtp.RCError(mtp.RC_IncompleteTransfer)) {
			return err
		}
		if err == nil && digest == p.Digest {
			return s.commitUpload(p, id)
		}
	}
	// Only this journal's exact random-name object may be removed on retry.
	if _, err := s.writable(storage, true); err != nil {
		return err
	}
	if _, err := s.revalidate(id); err != nil {
		return err
	}
	if err := s.usb.DeleteObject(id); err != nil {
		return err
	}
	delete(s.objects, id)
	delete(s.paths, id)
	return os.Remove(s.journalPath(storage))
}

func finalFilename(local, digest string) string {
	ext := strings.ToLower(filepath.Ext(local))
	stem := []rune(strings.TrimSuffix(filepath.Base(local), filepath.Ext(local)))
	if len(stem) > 64 {
		stem = stem[:64]
	}
	clean := strings.Map(func(r rune) rune {
		if r < 32 || strings.ContainsRune("/\\:*?\"<>|", r) {
			return '_'
		}
		return r
	}, string(stem))
	return clean + "-" + digest + ext
}

func (s *session) upload(storage uint32, local, digest string, size uint64) (*Song, error) {
	// This transport uses the classic 32-bit SendObject container length.
	if size > math.MaxUint32-12 {
		return nil, errors.New("audio file exceeds the 4 GiB MTP upload limit")
	}
	space, err := s.writable(storage, false)
	if err != nil {
		return nil, err
	}
	if space.FreeSpaceInBytes < size {
		return nil, fmt.Errorf("insufficient device space: need %d bytes, have %d", size, space.FreeSpaceInBytes)
	}
	parent, err := s.managedDirectory(storage)
	if err != nil {
		return nil, err
	}
	var token [16]byte
	if _, err := rand.Read(token[:]); err != nil {
		return nil, err
	}
	p := pendingUpload{DeviceID: s.device.ID, StorageID: storage, Parent: parent, Temporary: ".tyranny-" + hex.EncodeToString(token[:]) + ".tyranny-partial", Final: finalFilename(local, digest), Digest: digest, Size: size}
	if _, exists, err := s.objectAt(storage, path.Join("/Music/Tyranny", p.Final)); err != nil {
		return nil, err
	} else if exists {
		return nil, errors.New("destination filename exists with different content; refusing overwrite")
	}
	file, err := os.Open(local)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	stat, err := file.Stat()
	if err != nil {
		return nil, err
	}
	if !stat.Mode().IsRegular() || uint64(stat.Size()) != size {
		return nil, errors.New("source changed before upload")
	}
	if err := durableJSON(s.journalPath(storage), p); err != nil {
		return nil, err
	}
	if _, err := s.writable(storage, false); err != nil {
		return nil, err
	}
	if _, err := s.revalidate(parent); err != nil {
		return nil, err
	}
	format, _ := audioFormat(local)
	info := mtp.ObjectInfo{StorageID: storage, ParentObject: parent, Filename: p.Temporary, ObjectFormat: format, CompressedSize: uint32(size), ModificationDate: time.Now()}
	gotStorage, gotParent, id, err := s.usb.SendObjectInfo(storage, parent, &info)
	if err != nil {
		return nil, fmt.Errorf("start upload: %w", err)
	}
	if gotStorage != storage || gotParent != parent {
		return nil, errors.New("device chose unexpected upload location")
	}
	if err := s.usb.SendObject(io.LimitReader(file, int64(size)), int64(size), mtp.EmptyProgressFunc); err != nil {
		return nil, fmt.Errorf("upload interrupted; retry to recover: %w", err)
	}
	var actual mtp.ObjectInfo
	if err := s.usb.GetObjectInfo(id, &actual); err != nil {
		return nil, err
	}
	if actual.StorageID != storage || actual.ParentObject != parent || actual.Filename != p.Temporary || uint64(actual.CompressedSize) != size {
		return nil, errors.New("uploaded staging object metadata mismatch")
	}
	s.objects[id], s.paths[id] = actual, path.Join("/Music/Tyranny", p.Temporary)
	copiedDigest, err := s.objectDigest(id, size)
	if err != nil {
		return nil, err
	}
	if copiedDigest != digest {
		return nil, errors.New("uploaded content checksum mismatch; retry to recover")
	}
	if err := s.commitUpload(p, id); err != nil {
		return nil, err
	}
	return &s.device.Songs[len(s.device.Songs)-1], nil
}

func (s *session) syncFiles(storage uint32, files []LocalFile, response *Response) error {
	if _, err := s.writable(storage, false); err != nil {
		return err
	}
	if err := s.recoverUpload(storage); err != nil {
		return fmt.Errorf("recover interrupted upload: %w", err)
	}
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
			response.Skipped++
		} else {
			song, err = s.upload(storage, file.Path, digest, size)
			if err != nil {
				return fmt.Errorf("%s: %w", file.Path, err)
			}
			response.Copied++
		}
		response.Matches = append(response.Matches, Match{file.Path, song.ID, song.StorageID, digest})
	}
	return nil
}
