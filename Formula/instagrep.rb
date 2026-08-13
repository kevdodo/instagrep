# Homebrew formula for `instagrep`.
#
# Until the first versioned release ships prebuilt bottles, install from HEAD:
#   brew tap kvyaswanth/instagrep
#   brew install --HEAD instagrep
#
# When the first `v*` tag is pushed, the release workflow produces a tarball;
# fill in `url`/`sha256` below (run `shasum -a 256` on the tarball) and drop
# the `head` line to enable plain `brew install instagrep`.

class Instagrep < Formula
  desc "Fast grep-style search using a persisted roaring-bitmap trigram index"
  homepage "https://github.com/kvyaswanth/instagrep"
  license "MIT"
  head "https://github.com/kvyaswanth/instagrep.git", branch: "master"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    (testpath / "hello.rs").write("fn main() { println!(\"hello\"); }\n")
    assert_match "fn main", shell_output("#{bin}/instagrep 'fn main' #{testpath}")
  end
end
