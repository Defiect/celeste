// Celeste's combined Go archive. Exposes both rclone's librclone RPC
// surface (replacement for the upstream librclone-sys crate) and our
// native ProtonDrive client's C entry points.
//
// Built as:
//
//	go build --buildmode=c-archive -o libceleste_native.a .
//
// One Go runtime, both feature sets. See the integration plan for
// why we can't have two independent Go archives coexisting in the
// Celeste binary.
package main

/*
#include <stdlib.h>

struct RcloneRPCResult {
	char*	Output;
	int	Status;
};
*/
import "C"

import (
	"unsafe"

	"github.com/rclone/rclone/librclone/librclone"

	// Pull in the same rclone surface librclone-sys does, MINUS the
	// protondrive backend. Rclone's protondrive backend links in
	// henrybear327/go-proton-api (a fork pinned to an older resty
	// API) which collides with ProtonMail/go-proton-api + Proton's
	// resty fork that our native Drive layer needs. We own the
	// ProtonDrive path via native-go/drive/ (arriving in Phase 2+)
	// so dropping rclone's protondrive costs us nothing.
	//
	// This is an explicit expansion of github.com/rclone/rclone/backend/all
	// with the one import excluded. Keep in sync with upstream
	// /backend/all/all.go if we bump rclone.
	_ "github.com/rclone/rclone/backend/alias"
	_ "github.com/rclone/rclone/backend/azureblob"
	_ "github.com/rclone/rclone/backend/azurefiles"
	_ "github.com/rclone/rclone/backend/b2"
	_ "github.com/rclone/rclone/backend/box"
	_ "github.com/rclone/rclone/backend/cache"
	_ "github.com/rclone/rclone/backend/chunker"
	_ "github.com/rclone/rclone/backend/cloudinary"
	_ "github.com/rclone/rclone/backend/combine"
	_ "github.com/rclone/rclone/backend/compress"
	_ "github.com/rclone/rclone/backend/crypt"
	_ "github.com/rclone/rclone/backend/drive"
	_ "github.com/rclone/rclone/backend/dropbox"
	_ "github.com/rclone/rclone/backend/fichier"
	_ "github.com/rclone/rclone/backend/filefabric"
	_ "github.com/rclone/rclone/backend/filescom"
	_ "github.com/rclone/rclone/backend/ftp"
	_ "github.com/rclone/rclone/backend/gofile"
	_ "github.com/rclone/rclone/backend/googlecloudstorage"
	_ "github.com/rclone/rclone/backend/googlephotos"
	_ "github.com/rclone/rclone/backend/hasher"
	_ "github.com/rclone/rclone/backend/hdfs"
	_ "github.com/rclone/rclone/backend/hidrive"
	_ "github.com/rclone/rclone/backend/http"
	_ "github.com/rclone/rclone/backend/iclouddrive"
	_ "github.com/rclone/rclone/backend/imagekit"
	_ "github.com/rclone/rclone/backend/internetarchive"
	_ "github.com/rclone/rclone/backend/jottacloud"
	_ "github.com/rclone/rclone/backend/koofr"
	_ "github.com/rclone/rclone/backend/linkbox"
	_ "github.com/rclone/rclone/backend/local"
	_ "github.com/rclone/rclone/backend/mailru"
	_ "github.com/rclone/rclone/backend/mega"
	_ "github.com/rclone/rclone/backend/memory"
	_ "github.com/rclone/rclone/backend/netstorage"
	_ "github.com/rclone/rclone/backend/onedrive"
	_ "github.com/rclone/rclone/backend/opendrive"
	_ "github.com/rclone/rclone/backend/oracleobjectstorage"
	_ "github.com/rclone/rclone/backend/pcloud"
	_ "github.com/rclone/rclone/backend/pikpak"
	_ "github.com/rclone/rclone/backend/pixeldrain"
	_ "github.com/rclone/rclone/backend/premiumizeme"
	// protondrive intentionally omitted — replaced by native Go layer
	_ "github.com/rclone/rclone/backend/putio"
	_ "github.com/rclone/rclone/backend/qingstor"
	_ "github.com/rclone/rclone/backend/quatrix"
	_ "github.com/rclone/rclone/backend/s3"
	_ "github.com/rclone/rclone/backend/seafile"
	_ "github.com/rclone/rclone/backend/sftp"
	_ "github.com/rclone/rclone/backend/sharefile"
	_ "github.com/rclone/rclone/backend/sia"
	_ "github.com/rclone/rclone/backend/smb"
	_ "github.com/rclone/rclone/backend/storj"
	_ "github.com/rclone/rclone/backend/sugarsync"
	_ "github.com/rclone/rclone/backend/swift"
	_ "github.com/rclone/rclone/backend/ulozto"
	_ "github.com/rclone/rclone/backend/union"
	_ "github.com/rclone/rclone/backend/uptobox"
	_ "github.com/rclone/rclone/backend/webdav"
	_ "github.com/rclone/rclone/backend/yandex"
	_ "github.com/rclone/rclone/backend/zoho"

	_ "github.com/rclone/rclone/cmd/cmount"
	_ "github.com/rclone/rclone/cmd/mount"
	_ "github.com/rclone/rclone/cmd/mount2"
	_ "github.com/rclone/rclone/fs/operations"
	_ "github.com/rclone/rclone/fs/sync"
	_ "github.com/rclone/rclone/lib/plugin"

	// Proton-API-Bridge stays in the tree at native-go/proton-bridge as
	// read-only reference material — NOT imported here. Our own Drive
	// layer (native-go/drive/, arriving in Phase 2+) imports go-proton-api
	// directly.
	"github.com/ProtonMail/go-proton-api"
)

// ---------------- librclone ABI ----------------
// These functions mirror upstream librclone-sys's C surface so any
// existing Rust caller (celeste::infrastructure::rclone) keeps working
// unchanged after we swap the crate dep.

//export RcloneInitialize
func RcloneInitialize() {
	librclone.Initialize()
}

//export RcloneFinalize
func RcloneFinalize() {
	librclone.Finalize()
}

// RcloneRPCResult mirrors the struct librclone-sys consumes. Must stay
// layout-compatible; the Rust side pattern-matches on it.
type RcloneRPCResult struct { //nolint:deadcode,unused
	Output *C.char
	Status C.int
}

//export RcloneRPC
func RcloneRPC(method *C.char, input *C.char) (result C.struct_RcloneRPCResult) {
	output, status := librclone.RPC(C.GoString(method), C.GoString(input))
	result.Output = C.CString(output)
	result.Status = C.int(status)
	return result
}

//export RcloneFreeString
func RcloneFreeString(str *C.char) {
	C.free(unsafe.Pointer(str))
}

// ---------------- ProtonDrive native surface ----------------
// Phase 1 exposes just a version ping so the Rust side can verify the
// combined archive linked correctly and both code paths are callable.
// Later phases (session, list, upload, download, trash) layer their
// own //export functions here.

// ProtonDrive_Version returns a short identity string proving the
// go-proton-api dependency linked into our archive. Caller must free
// the result with RcloneFreeString (same allocator).
//
//export ProtonDrive_Version
func ProtonDrive_Version() *C.char {
	// Build a fresh Manager just to exercise the symbol path; we throw
	// it away immediately. No network traffic.
	_ = proton.New(proton.WithAppVersion("celeste-native/0.0.0"))
	return C.CString("celeste-native proton-api bound")
}

// main is required by cgo for c-archive builds; body intentionally empty.
func main() {}
