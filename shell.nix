# Nix dev-shell for building Celeste on NixOS without a global toolchain
# install. Enter with `nix-shell` (or `nix-shell --run 'just build'`).
{ pkgs ? import <nixpkgs> { } }:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    just
    pkg-config
    rustup
    go
    rustPlatform.bindgenHook # sets LIBCLANG_PATH + BINDGEN_EXTRA_CLANG_ARGS for librclone-sys
  ];

  # Iced runtime libraries (winit/wgpu pick these at launch) plus librclone's
  # OpenSSL + the rclone CLI.
  buildInputs = with pkgs; [
    libxkbcommon
    vulkan-loader
    wayland
    xorg.libX11
    xorg.libXcursor
    xorg.libXi
    xorg.libXrandr
    fontconfig
    openssl
    rclone
  ];

  # Tell winit where to find the Wayland / Vulkan shared libs at run time.
  LD_LIBRARY_PATH = with pkgs;
    lib.makeLibraryPath [
      libxkbcommon
      vulkan-loader
      wayland
      xorg.libX11
      xorg.libXcursor
      xorg.libXi
      xorg.libXrandr
      fontconfig
    ];
}
