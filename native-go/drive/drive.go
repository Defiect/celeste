package drive

// Drive bootstrap: finds the user's active volume + primary share,
// decrypts the main share keyring, fetches the root link. Attached
// to Session at Login/Resume time so every later Drive call has the
// state it needs without re-querying Proton.
//
// Ported and trimmed from Proton-API-Bridge/drive.go + shares.go +
// volumes.go. Dropped Bridge's cache layer (we recompute; deferring
// caching until Phase 3+ performance pass) and the Config struct
// (everything Bridge threaded through options we now pin at
// package-scope constants).

import (
	"context"
	"errors"

	"github.com/ProtonMail/go-proton-api"
	"github.com/ProtonMail/gopenpgp/v2/crypto"
)

// Errors surfaced during Drive bootstrap.
var (
	ErrNoActiveVolume             = errors.New("account has no active Proton Drive volume")
	ErrMainSharePreconditionFail  = errors.New("main share integrity check failed (unexpected share layout)")
	ErrAddressKeyringMissing      = errors.New("address keyring for main share not present")
)

// bootstrapDrive populates the Drive-state fields on a Session: main
// share, root link, and the decrypted main-share keyring. Called
// once from Login() and Resume(); failure here tears the session
// back down.
func (s *Session) bootstrapDrive(ctx context.Context) error {
	volumes, err := s.c.ListVolumes(ctx)
	if err != nil {
		return err
	}
	var mainShareID string
	for i := range volumes {
		if volumes[i].State == proton.VolumeStateActive {
			mainShareID = volumes[i].Share.ShareID
			break
		}
	}
	if mainShareID == "" {
		return ErrNoActiveVolume
	}

	mainShare, err := s.c.GetShare(ctx, mainShareID)
	if err != nil {
		return err
	}

	// Mirror Bridge's integrity check: the main share must be the
	// primary share of type "main" for our assumptions to hold.
	shares, err := s.c.ListShares(ctx, true)
	if err != nil {
		return err
	}
	mainShareOK := false
	for _, m := range shares {
		if m.ShareID == mainShare.ShareID &&
			m.LinkID == mainShare.LinkID &&
			m.Flags == proton.PrimaryShare &&
			m.Type == proton.ShareTypeMain {
			mainShareOK = true
			break
		}
	}
	if !mainShareOK {
		return ErrMainSharePreconditionFail
	}

	addrKR, ok := s.addrKRs[mainShare.AddressID]
	if !ok {
		return ErrAddressKeyringMissing
	}
	mainShareKR, err := mainShare.GetKeyRing(addrKR)
	if err != nil {
		return err
	}

	rootLink, err := s.c.GetLink(ctx, mainShare.ShareID, mainShare.LinkID)
	if err != nil {
		return err
	}

	s.mainShare = &mainShare
	s.mainShareKR = mainShareKR
	s.defaultAddrKR = addrKR
	s.rootLink = &rootLink
	s.signatureAddress = mainShare.Creator
	return nil
}

// linkKR returns `link`'s own unlocked node keyring — the one that
// decrypts `link`'s children's names, node keys, etc. For the root
// link the parent KR is the main share KR; otherwise we walk the
// parent chain. Matches Bridge's `_getLinkKR`.
func (s *Session) linkKR(ctx context.Context, link *proton.Link) (*crypto.KeyRing, error) {
	var parentKR *crypto.KeyRing
	if link.ParentLinkID == "" {
		parentKR = s.mainShareKR
	} else {
		pkr, err := s.linkKRByID(ctx, link.ParentLinkID)
		if err != nil {
			return nil, err
		}
		parentKR = pkr
	}
	return link.GetKeyRing(parentKR, s.defaultAddrKR)
}

// linkKRByID is `linkKR`'s "I only have the ID" variant — fetches
// the link and recurses. The special case for `linkID == ""` is
// Bridge's convention: a root link's stored ParentLinkID is "", and
// that resolves to the main share keyring. No caching yet; we'll
// add it back in a later pass once the read path is solid against
// live traffic.
func (s *Session) linkKRByID(ctx context.Context, linkID string) (*crypto.KeyRing, error) {
	if linkID == "" {
		return s.mainShareKR, nil
	}
	link, err := s.c.GetLink(ctx, s.mainShare.ShareID, linkID)
	if err != nil {
		return nil, err
	}
	return s.linkKR(ctx, &link)
}

// getLink is the single shared point for "fetch link metadata by ID".
// Later phases (upload conflict handling, cached trees) layer caching
// here.
func (s *Session) getLink(ctx context.Context, linkID string) (proton.Link, error) {
	return s.c.GetLink(ctx, s.mainShare.ShareID, linkID)
}
