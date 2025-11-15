class Cuenv < Formula
  desc "Modern application build toolchain with typed environments and CUE-powered task orchestration"
  homepage "https://github.com/cuenv/cuenv"
  url "https://github.com/cuenv/cuenv/archive/refs/tags/v0.1.1.tar.gz"
  sha256 "7822682bc556a8c9819f584c76185ea511a877287be9b7cbf9df3fc3c5d4273b"
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
