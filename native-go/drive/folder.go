package drive

// Drive read surface: listing children + statting a link.
//
// Ported and simplified from Proton-API-Bridge/folder.go. Two
// deliberate deviations from Bridge:
//
//   1. Bridge threads a `signatureVerificationKeyring` composed from
//      the signer's email address through every GetName / GetKeyRing
//      call. Upstream Link doesn't expose `SignatureEmail` /
//      `NameSignatureEmail` — those fields only exist on
//      RevisionMetadata. For single-account usage (our case) the
//      owner's default address keyring is always the correct
//      verifier, which is what we pass.
//   2. Child links fetched during listings are cached on the Session's
//      linkCache so that recursive listings and subsequent getLink /
//      linkKR calls hit the cache instead of re-fetching each link
//      individually.
//
// Bridge's original `folder.go::ListDirectory` filters out
// non-active children (drafts, trashed, deleted). We do the same.

import (
	"context"
	"time"

	"github.com/ProtonMail/go-proton-api"
)

// Entry is the per-child record returned by ListDirectory / Stat.
// Shapes to match Celeste's `RemoteItem` domain type across the FFI
// boundary without the domain crate pulling in anything Proton-
// specific.
type Entry struct {
	LinkID       string `json:"link_id"`
	ParentLinkID string `json:"parent_link_id"`
	Name         string `json:"name"`
	IsDir        bool   `json:"is_dir"`
	Size         int64  `json:"size"`
	ModTimeUnix  int64  `json:"mod_time_unix"`
	MIMEType     string `json:"mime_type,omitempty"`
}

func entryFromLink(link *proton.Link, name string) *Entry {
	modTime := link.ModifyTime
	if modTime == 0 {
		modTime = link.CreateTime
	}
	if modTime == 0 {
		// Fall back to the wall clock so the sync algorithm's mtime
		// comparisons don't see zero-valued timestamps when a link
		// hasn't been modified since creation.
		modTime = time.Now().Unix()
	}
	return &Entry{
		LinkID:       link.LinkID,
		ParentLinkID: link.ParentLinkID,
		Name:         name,
		IsDir:        link.Type == proton.LinkTypeFolder,
		Size:         link.Size,
		ModTimeUnix:  modTime,
		MIMEType:     link.MIMEType,
	}
}

// ListDirectory returns the active children of `folderLinkID`. The
// caller can pass the empty string to list the root.
func (s *Session) ListDirectory(ctx context.Context, folderLinkID string) ([]*Entry, error) {
	if folderLinkID == "" {
		folderLinkID = s.RootLinkID()
	}
	folderLink, err := s.getLink(ctx, folderLinkID)
	if err != nil {
		return nil, err
	}
	if folderLink.State != proton.LinkStateActive {
		return nil, nil
	}

	childrenLinks, err := s.c.ListChildren(ctx, s.mainShare.ShareID, folderLink.LinkID, true)
	if err != nil {
		return nil, err
	}

	// The children's names are encrypted to the *folder's* node
	// keyring, so we unlock that once and reuse.
	folderKR, err := s.linkKR(ctx, &folderLink)
	if err != nil {
		return nil, err
	}

	out := make([]*Entry, 0, len(childrenLinks))
	for i := range childrenLinks {
		child := &childrenLinks[i]
		// Cache every child's link metadata so recursive listings
		// hit the cache instead of re-fetching each link individually.
		s.linkCache[child.LinkID] = *child
		if child.State != proton.LinkStateActive {
			continue
		}
		name, err := child.GetName(folderKR, s.defaultAddrKR)
		if err != nil {
			return nil, err
		}
		out = append(out, entryFromLink(child, name))
	}
	return out, nil
}

// Stat returns the metadata for a single link, with the name decrypted
// via the parent's keyring. Returns nil without error when the link
// exists but is not in the active state — mirrors the semantics the
// sync engine expects from its `stat` port.
func (s *Session) Stat(ctx context.Context, linkID string) (*Entry, error) {
	if linkID == "" {
		linkID = s.RootLinkID()
	}
	link, err := s.getLink(ctx, linkID)
	if err != nil {
		return nil, err
	}
	if link.State != proton.LinkStateActive {
		return nil, nil
	}
	var name string
	if link.ParentLinkID == "" {
		// Root link has no encrypted name — it's named by the share,
		// not by the parent's keyring. Return a stable placeholder.
		name = ""
	} else {
		parentKR, err := s.linkKRByID(ctx, link.ParentLinkID)
		if err != nil {
			return nil, err
		}
		name, err = link.GetName(parentKR, s.defaultAddrKR)
		if err != nil {
			return nil, err
		}
	}
	return entryFromLink(&link, name), nil
}
