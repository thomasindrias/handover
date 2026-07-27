# Updated automatically by .github/workflows/release.yml on each tagged release.
class Sesh < Formula
  desc "Local-first session layer for coding agents"
  homepage "https://github.com/thomasindrias/sesh"
  version "0.1.0"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/thomasindrias/sesh/releases/download/v0.1.0/sesh-0.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/thomasindrias/sesh/releases/download/v0.1.0/sesh-0.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/thomasindrias/sesh/releases/download/v0.1.0/sesh-0.1.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/thomasindrias/sesh/releases/download/v0.1.0/sesh-0.1.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "sesh"
  end

  test do
    assert_match "sesh #{version}", shell_output("#{bin}/sesh --version")
  end
end
