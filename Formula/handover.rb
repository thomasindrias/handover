# Updated automatically by .github/workflows/release.yml on each tagged release.
class Sesh < Formula
  desc "Local-first session layer for coding agents"
  homepage "https://github.com/thomasindrias/sesh"
  version "0.1.0"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/thomasindrias/handover/releases/download/v0.1.0/handover-0.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "21c0186a43afc8e458d486c95c367cf9677880c6e39c4281aaaa7729d548c288"
    end
    on_intel do
      url "https://github.com/thomasindrias/handover/releases/download/v0.1.0/handover-0.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "6e8361558f9e9cd3cff376035d76018289b0cf540b53683b22e4fe467706c3b5"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/thomasindrias/handover/releases/download/v0.1.0/handover-0.1.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "2597008ea4549c4c3ab604861a10d8ec5d5a5cfc7f3b181d753b4220d88af3f1"
    end
    on_intel do
      url "https://github.com/thomasindrias/handover/releases/download/v0.1.0/handover-0.1.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "4711be82b28721a7146cd7d63a12f74ed1a9839d7428e249afd9c56cecde87f2"
    end
  end

  def install
    bin.install "sesh"
  end

  test do
    assert_match "sesh #{version}", shell_output("#{bin}/sesh --version")
  end
end
