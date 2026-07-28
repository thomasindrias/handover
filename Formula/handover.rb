# Updated automatically by .github/workflows/release.yml on each tagged release.
class Sesh < Formula
  desc "Local-first session layer for coding agents"
  homepage "https://github.com/thomasindrias/sesh"
  version "0.1.1"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/thomasindrias/handover/releases/download/v0.1.1/handover-0.1.1-aarch64-apple-darwin.tar.gz"
      sha256 "94f1b5172eca321e630ed4b6e2f6052f0e620ba2c3678398781eddb8cd7676d8"
    end
    on_intel do
      url "https://github.com/thomasindrias/handover/releases/download/v0.1.1/handover-0.1.1-x86_64-apple-darwin.tar.gz"
      sha256 "8e3f55c1c98c1c77443abd91c03fa3c4f772b648191222549e6b395b6b28248f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/thomasindrias/handover/releases/download/v0.1.1/handover-0.1.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "9ae658cb0abf33866e151af008350aaa0534df23a9fbb2afe74d327e73b1f02c"
    end
    on_intel do
      url "https://github.com/thomasindrias/handover/releases/download/v0.1.1/handover-0.1.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "32b35e170bf3f52ffc2ebd316d9d8936cb569ad7a34f590d2ac0641622bcd150"
    end
  end

  def install
    bin.install "sesh"
  end

  test do
    assert_match "sesh #{version}", shell_output("#{bin}/sesh --version")
  end
end
