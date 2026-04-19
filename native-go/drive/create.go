package drive

// Create: mkdir + file uploads.
//
// Ported and trimmed from Proton-API-Bridge:
//   - folder.go::CreateNewFolder → CreateFolder
//   - file_upload.go::createFileUploadDraft + uploadAndCollectBlockData
//     + commitNewRevision → UploadFile
//
// CreateFolder handles the "already exists" conflict (Proton error
// code 2500, HTTP 422) by looking up the existing folder and returning
// its link ID, making the operation idempotent. File upload conflicts
// are not yet handled — the 422 is surfaced as-is to the caller.

import (
	"context"
	"crypto/sha1"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"io"
	"mime"
	"os"
	"path/filepath"
	"time"

	"github.com/ProtonMail/go-proton-api"
	"github.com/ProtonMail/gopenpgp/v2/crypto"
	"github.com/go-resty/resty/v2"
)

// Block / batch sizes lifted from Bridge. 4 MB blocks match Proton's
// on-disk chunking; 8 blocks per batch matches their concurrency
// sweet spot.
const (
	uploadBlockSize      = 4 * 1024 * 1024
	uploadBatchBlockSize = 8
)

// codeAlreadyExists is Proton's server-side error for "a file or
// folder with that name already exists" (HTTP 422).
const codeAlreadyExists = proton.Code(2500)

// ErrParentNotFolder surfaces when a mkdir / upload target parent
// turns out to be a file. The sync engine enforces this pre-flight
// anyway; this is belt-and-braces.
var ErrParentNotFolder = errors.New("parent link is not a folder")

// CreateFolder creates a new subfolder named `name` under
// `parentLinkID`. Empty parent = session root. Returns the new
// folder's link ID. Idempotent: if a folder with the same name
// already exists, its link ID is returned without hitting the
// create endpoint (avoids noisy 422 / Code=2500 from the server).
func (s *Session) CreateFolder(ctx context.Context, parentLinkID, name string) (string, error) {
	if parentLinkID == "" {
		parentLinkID = s.RootLinkID()
	}

	// Fast path: if a folder with this name already exists under the
	// parent, return its link ID immediately. This avoids the 422
	// round-trip (and resty's WARN/ERROR log noise) plus the wasted
	// crypto work of generating throwaway node keys.
	if existing, err := s.findChildByName(ctx, parentLinkID, name, true); err == nil && existing != "" {
		return existing, nil
	}

	parentLink, err := s.getLink(ctx, parentLinkID)
	if err != nil {
		return "", err
	}
	if parentLink.Type != proton.LinkTypeFolder {
		return "", ErrParentNotFolder
	}
	parentNodeKR, err := s.linkKR(ctx, &parentLink)
	if err != nil {
		return "", err
	}
	parentHashKey, err := parentLink.GetHashKey(parentNodeKR)
	if err != nil {
		return "", err
	}

	nodeKey, nodePassphraseEnc, nodePassphraseSig, err := generateNodeKeys(parentNodeKR, s.defaultAddrKR)
	if err != nil {
		return "", err
	}

	req := proton.CreateFolderReq{
		ParentLinkID: parentLink.LinkID,

		NodeKey:                 nodeKey,
		NodePassphrase:          nodePassphraseEnc,
		NodePassphraseSignature: nodePassphraseSig,

		SignatureAddress: s.signatureAddress,
	}
	if err := req.SetName(name, s.defaultAddrKR, parentNodeKR); err != nil {
		return "", err
	}
	if err := req.SetHash(name, parentHashKey); err != nil {
		return "", err
	}
	// The new folder's own hash key is independent of its parent's;
	// it's used to hash the names of children we create inside it.
	newFolderKR, err := getKeyRing(parentNodeKR, s.defaultAddrKR, nodeKey, nodePassphraseEnc, nodePassphraseSig)
	if err != nil {
		return "", err
	}
	_ = newFolderKR // only needed once we want the hash key back locally
	if err := req.SetNodeHashKey(newFolderKR); err != nil {
		return "", err
	}

	res, err := s.c.CreateFolder(ctx, s.mainShare.ShareID, req)
	if err != nil {
		// Handle "already exists" — look up the existing folder and
		// return its link ID, making CreateFolder idempotent.
		var apiErr *proton.APIError
		if errors.As(err, &apiErr) && apiErr.Code == codeAlreadyExists {
			existing, lookupErr := s.findChildByName(ctx, parentLink.LinkID, name, true)
			if lookupErr == nil && existing != "" {
				return existing, nil
			}
		}
		return "", err
	}
	return res.ID, nil
}

// findChildByName lists the active children of parentLinkID and returns
// the link ID of the first child matching `name` with the given type
// (isDir=true for folders, false for files). Returns ("", nil) when no
// match is found.
func (s *Session) findChildByName(ctx context.Context, parentLinkID, name string, isDir bool) (string, error) {
	entries, err := s.ListDirectory(ctx, parentLinkID)
	if err != nil {
		return "", err
	}
	for _, e := range entries {
		if e.Name == name && e.IsDir == isDir {
			return e.LinkID, nil
		}
	}
	return "", nil
}

// UploadFile uploads `srcPath`'s contents as a new file named `name`
// under `parentLinkID`. Uses the file's mtime for the revision's
// XAttr ModificationTime. Returns the new file's link ID.
//
// Flow (mirrors Bridge's uploadFile):
//   1. Open source file, get size + mtime.
//   2. Create draft on the server (CreateFile with encrypted name /
//      hash / session-key packet).
//   3. Read the file in 4 MB chunks: encrypt + detached-sign each
//      chunk, batch 8 blocks, RequestBlockUpload + UploadBlock for
//      the batch. Accumulate manifest-hash (SHA-256 of encrypted
//      blocks), XAttr block sizes, and SHA-1 of plaintext.
//   4. Commit the revision: sign the manifest, encrypt the XAttr
//      payload, send the CommitRevisionReq.
func (s *Session) UploadFile(ctx context.Context, parentLinkID, name, srcPath string) (string, error) {
	if parentLinkID == "" {
		parentLinkID = s.RootLinkID()
	}
	file, err := os.Open(srcPath)
	if err != nil {
		return "", err
	}
	defer file.Close()
	stat, err := file.Stat()
	if err != nil {
		return "", err
	}
	modTime := stat.ModTime()

	linkID, revisionID, sessionKey, nodeKR, err := s.createFileDraft(ctx, parentLinkID, name)
	if err != nil {
		return "", err
	}

	manifest, fileSize, blockSizes, sha1Hex, err := s.uploadBlocks(ctx, sessionKey, nodeKR, file, linkID, revisionID)
	if err != nil {
		// Best-effort draft cleanup. If this fails we swallow — the
		// CreateRevision path will retry the user's next upload and
		// the stale draft will be collected server-side eventually.
		_ = s.c.DeleteRevision(ctx, s.mainShare.ShareID, linkID, revisionID)
		return "", err
	}

	if err := s.commitRevision(ctx, nodeKR, linkID, revisionID, manifest, fileSize, blockSizes, sha1Hex, modTime); err != nil {
		_ = s.c.DeleteRevision(ctx, s.mainShare.ShareID, linkID, revisionID)
		return "", err
	}
	return linkID, nil
}

// createFileDraft posts a `CreateFile` request for a new file under
// `parentLinkID`. Returns (linkID, revisionID, fileSessionKey,
// fileNodeKR).
func (s *Session) createFileDraft(
	ctx context.Context,
	parentLinkID, name string,
) (string, string, *crypto.SessionKey, *crypto.KeyRing, error) {
	parentLink, err := s.getLink(ctx, parentLinkID)
	if err != nil {
		return "", "", nil, nil, err
	}
	if parentLink.Type != proton.LinkTypeFolder {
		return "", "", nil, nil, ErrParentNotFolder
	}
	parentNodeKR, err := s.linkKR(ctx, &parentLink)
	if err != nil {
		return "", "", nil, nil, err
	}
	parentHashKey, err := parentLink.GetHashKey(parentNodeKR)
	if err != nil {
		return "", "", nil, nil, err
	}

	nodeKey, nodePassphraseEnc, nodePassphraseSig, err := generateNodeKeys(parentNodeKR, s.defaultAddrKR)
	if err != nil {
		return "", "", nil, nil, err
	}

	mimeType := mime.TypeByExtension(filepath.Ext(name))
	if mimeType == "" {
		mimeType = "application/octet-stream"
	}

	req := proton.CreateFileReq{
		ParentLinkID:            parentLink.LinkID,
		MIMEType:                mimeType,
		NodeKey:                 nodeKey,
		NodePassphrase:          nodePassphraseEnc,
		NodePassphraseSignature: nodePassphraseSig,
		SignatureAddress:        s.signatureAddress,
	}
	if err := req.SetName(name, s.defaultAddrKR, parentNodeKR); err != nil {
		return "", "", nil, nil, err
	}
	if err := req.SetHash(name, parentHashKey); err != nil {
		return "", "", nil, nil, err
	}

	// Build the file's own node keyring so we can generate its
	// content session key and sign it.
	nodeKR, err := getKeyRing(parentNodeKR, s.defaultAddrKR, nodeKey, nodePassphraseEnc, nodePassphraseSig)
	if err != nil {
		return "", "", nil, nil, err
	}
	sessionKey, err := req.SetContentKeyPacketAndSignature(nodeKR)
	if err != nil {
		return "", "", nil, nil, err
	}

	res, err := s.c.CreateFile(ctx, s.mainShare.ShareID, req)
	if err != nil {
		return "", "", nil, nil, err
	}
	return res.ID, res.RevisionID, sessionKey, nodeKR, nil
}

// uploadBlocks reads `file` in 4 MB chunks, encrypts + signs each,
// batches `uploadBatchBlockSize` at a time, and uploads in parallel
// within each batch. Returns (manifestData, totalFileSize, blockSizes,
// sha1Hex).
func (s *Session) uploadBlocks(
	ctx context.Context,
	sessionKey *crypto.SessionKey,
	nodeKR *crypto.KeyRing,
	file io.Reader,
	linkID, revisionID string,
) ([]byte, int64, []int64, string, error) {
	type pendingBlock struct {
		info proton.BlockUploadInfo
		data []byte
	}

	var (
		totalSize  int64
		manifest   []byte
		blockSizes []int64
		sha1Sum    = sha1.New()
		pending    []pendingBlock
		blockIdx   = 1
	)

	flushBatch := func() error {
		if len(pending) == 0 {
			return nil
		}
		list := make([]proton.BlockUploadInfo, len(pending))
		for i := range pending {
			list[i] = pending[i].info
		}
		uploadReq := proton.BlockUploadReq{
			AddressID:  s.mainShare.AddressID,
			ShareID:    s.mainShare.ShareID,
			LinkID:     linkID,
			RevisionID: revisionID,
			BlockList:  list,
		}
		uploadLinks, err := s.c.RequestBlockUpload(ctx, uploadReq)
		if err != nil {
			return err
		}
		if len(uploadLinks) != len(pending) {
			return errors.New("block upload: server returned wrong number of upload links")
		}
		errCh := make(chan error, len(uploadLinks))
		for i := range uploadLinks {
			go func(i int) {
				stream := resty.NewByteMultipartStream(pending[i].data)
				errCh <- s.c.UploadBlock(ctx, uploadLinks[i].BareURL, uploadLinks[i].Token, stream)
			}(i)
		}
		for range uploadLinks {
			if err := <-errCh; err != nil {
				return err
			}
		}
		pending = pending[:0]
		return nil
	}

	buf := make([]byte, uploadBlockSize)
	for {
		n, readErr := io.ReadFull(file, buf)
		if n == 0 {
			if readErr == io.EOF || readErr == io.ErrUnexpectedEOF {
				break
			}
			if readErr != nil {
				return nil, 0, nil, "", readErr
			}
		}
		chunk := make([]byte, n)
		copy(chunk, buf[:n])
		totalSize += int64(n)
		blockSizes = append(blockSizes, int64(n))
		sha1Sum.Write(chunk)

		plain := crypto.NewPlainMessage(chunk)
		encData, err := sessionKey.Encrypt(plain)
		if err != nil {
			return nil, 0, nil, "", err
		}
		encSig, err := s.defaultAddrKR.SignDetachedEncrypted(plain, nodeKR)
		if err != nil {
			return nil, 0, nil, "", err
		}
		encSigArm, err := encSig.GetArmored()
		if err != nil {
			return nil, 0, nil, "", err
		}

		hash := sha256.Sum256(encData)
		manifest = append(manifest, hash[:]...)

		pending = append(pending, pendingBlock{
			info: proton.BlockUploadInfo{
				Index:        blockIdx,
				Size:         int64(len(encData)),
				EncSignature: encSigArm,
				Hash:         base64.StdEncoding.EncodeToString(hash[:]),
			},
			data: encData,
		})
		blockIdx++
		if len(pending) == uploadBatchBlockSize {
			if err := flushBatch(); err != nil {
				return nil, 0, nil, "", err
			}
		}
		if readErr == io.EOF || readErr == io.ErrUnexpectedEOF {
			break
		}
		if readErr != nil {
			return nil, 0, nil, "", readErr
		}
	}
	if err := flushBatch(); err != nil {
		return nil, 0, nil, "", err
	}
	return manifest, totalSize, blockSizes, hex.EncodeToString(sha1Sum.Sum(nil)), nil
}

// commitRevision signs the manifest, builds the XAttr payload,
// encrypts it, and PUTs the CommitRevisionReq.
func (s *Session) commitRevision(
	ctx context.Context,
	nodeKR *crypto.KeyRing,
	linkID, revisionID string,
	manifestData []byte,
	fileSize int64,
	blockSizes []int64,
	sha1Hex string,
	modTime time.Time,
) error {
	manifestSig, err := s.defaultAddrKR.SignDetached(crypto.NewPlainMessage(manifestData))
	if err != nil {
		return err
	}
	manifestSigArm, err := manifestSig.GetArmored()
	if err != nil {
		return err
	}
	req := proton.CommitRevisionReq{
		ManifestSignature: manifestSigArm,
		SignatureAddress:  s.signatureAddress,
	}
	xAttr := &proton.RevisionXAttrCommon{
		ModificationTime: modTime.UTC().Format("2006-01-02T15:04:05-0700"),
		Size:             fileSize,
		BlockSizes:       blockSizes,
		Digests:          map[string]string{"SHA1": sha1Hex},
	}
	if err := req.SetEncXAttrString(s.defaultAddrKR, nodeKR, xAttr); err != nil {
		return err
	}
	return s.c.CommitRevision(ctx, s.mainShare.ShareID, linkID, revisionID, req)
}
