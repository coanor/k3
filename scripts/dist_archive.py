"""将已准备的发行目录压缩为便携包，并生成配套校验文件。"""

import hashlib
from pathlib import Path
import tarfile
import tempfile
import zipfile


def write_archive(root: Path, archive: Path) -> None:
    """保留顶级目录和 Unix 相对链接，生成归档后替换目标文件。"""
    with tempfile.TemporaryDirectory(prefix="k3-archive-", dir=archive.parent) as directory:
        temporary = Path(directory) / archive.name
        if archive.suffix == ".zip":
            # ZIP64 避免 PowerShell Compress-Archive 的单文件 2GB 限制。
            with zipfile.ZipFile(temporary, "w", compression=zipfile.ZIP_DEFLATED,
                                 compresslevel=9) as stream:
                for path in sorted(root.rglob("*")):
                    if path.is_file():
                        stream.write(path, path.relative_to(root.parent))
        elif archive.name.endswith(".tar.gz"):
            with tarfile.open(temporary, "w:gz", compresslevel=6) as stream:
                stream.add(root, arcname=root.name)
        else:
            raise ValueError(f"Unsupported archive format: {archive.name}")
        if temporary.stat().st_size >= 2 * 1024**3:
            raise ValueError("Package exceeds the GitHub Release 2 GiB limit; reduce additional models")
        with temporary.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        checksum = temporary.with_name(temporary.name + ".sha256")
        checksum.write_text(f"{digest}  {archive.name}\n", encoding="ascii")
        temporary.replace(archive)
        checksum.replace(archive.with_name(archive.name + ".sha256"))
