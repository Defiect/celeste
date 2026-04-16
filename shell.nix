{ pkgs ? import <nixpkgs> { } }:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    just
    pkg-config
    rustup
    go
    rustPlatform.bindgenHook # sets LIBCLANG_PATH + BINDGEN_EXTRA_CLANG_ARGS for librclone-sys
  ];

  buildInputs = with pkgs; [
    cairo
    dbus
    gdk-pixbuf
    glib
    graphene
    gtk4
    libadwaita
    pango
    openssl
    rclone
  ];
}
