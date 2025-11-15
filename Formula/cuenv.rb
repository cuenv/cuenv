class Cuenv < Formula
  desc "Modern application build toolchain with typed environments and CUE-powered task orchestration"
  homepage "https://github.com/cuenv/cuenv"
  url "https://github.com/cuenv/cuenv/archive/refs/tags/v0.1.1.tar.gz"
  sha256 ""  # To be filled when release is published
  license "AGPL-3.0-or-later"
  head "https://github.com/cuenv/cuenv.git", branch: "main"

  depends_on "rust" => :build
  depends_on "go" => :build

  def install
    # Build the cuenv-cli binary
    system "cargo", "build", "--release", "--package", "cuenv-cli"

    # Install the binary
    bin.install "target/release/cuenv"
  end

  test do
    # Test that the binary exists and runs
    system "#{bin}/cuenv", "--version"
  end
end
