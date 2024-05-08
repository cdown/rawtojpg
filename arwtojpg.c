#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#define expect(x)                                                              \
    do {                                                                       \
        if (!(x)) {                                                            \
            fprintf(stderr, "FATAL: !(%s) at %s:%s:%d\n", #x, __FILE__,        \
                    __func__, __LINE__);                                       \
            abort();                                                           \
        }                                                                      \
    } while (0)

#define snprintf_safe(buf, len, fmt, ...)                                      \
    do {                                                                       \
        int needed = snprintf(buf, len, fmt, __VA_ARGS__);                     \
        expect(needed >= 0 && (size_t)needed < (len));                         \
    } while (0)

// Reverse engineered from looking at a bunch of ARW files. Obviously not
// stable, tested on Sony a1 with 1.31 firmware. Can be extracted by iterating
// through EXIF, but that's much slower, and these are static.
#define OFFSET_POSITION 0x21c18
#define LENGTH_POSITION 0x21c24

static int is_jpeg_soi(const char *buf) {
    return buf[0] == (char)0xff && buf[1] == (char)0xd8;
}

static void extract_jpeg(int in_arw_fd, const char *filename, int out_dir_fd) {
    // Try to avoid reading more of the ARW than needed, we only need a very
    // specific piece.
    expect(posix_fadvise(in_arw_fd, 0, 0, POSIX_FADV_RANDOM) == 0);

    struct stat st;
    expect(fstat(in_arw_fd, &st) == 0);
    size_t file_size = (size_t)st.st_size;

    char *arw_buf = mmap(NULL, file_size, PROT_READ, MAP_PRIVATE, in_arw_fd, 0);
    expect(arw_buf != MAP_FAILED);
    expect(madvise(arw_buf, file_size, MADV_RANDOM) == 0);

    uint32_t jpeg_offset, jpeg_sz;
    memcpy(&jpeg_offset, arw_buf + OFFSET_POSITION, sizeof(uint32_t));
    memcpy(&jpeg_sz, arw_buf + LENGTH_POSITION, sizeof(uint32_t));

    expect(jpeg_offset + jpeg_sz <= file_size);
    expect(is_jpeg_soi(arw_buf + jpeg_offset));

    char basename[PATH_MAX];
    snprintf_safe(basename, sizeof(basename), "%s", filename);
    char *dot = strrchr(basename, '.');
    if (dot) {
        *dot = '\0';
    }

    char output_file[PATH_MAX];
    snprintf_safe(output_file, sizeof(output_file), "%s.jpg", basename);

    printf("%s\n", filename);

    int out_jpeg_fd =
        openat(out_dir_fd, output_file, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    expect(out_jpeg_fd >= 0);

    expect(write(out_jpeg_fd, arw_buf + jpeg_offset, jpeg_sz) ==
           (ssize_t)jpeg_sz);
    close(out_jpeg_fd);
    munmap(arw_buf, file_size);
}

static void process_directory(int in_dir_fd, int out_dir_fd) {
    DIR *dir = fdopendir(in_dir_fd);
    expect(dir);

    struct dirent *entry;
    while ((entry = readdir(dir))) {
        const char *filename = entry->d_name;
        size_t len = strlen(filename);
        if (len < 4 || strcmp(filename + len - 4, ".ARW") != 0) {
            continue;
        }
        int in_arw_fd = openat(in_dir_fd, entry->d_name, O_RDONLY);
        expect(in_arw_fd >= 0);
        extract_jpeg(in_arw_fd, entry->d_name, out_dir_fd);
        close(in_arw_fd);
    }
    closedir(dir);
}

int main(int argc, char *argv[]) {
    if (argc < 2) {
        fprintf(stderr, "Usage: %s input_dir [output_dir]\n", argv[0]);
        exit(1);
    }

    int in_dir_fd = open(argv[1], O_RDONLY | O_DIRECTORY);
    expect(in_dir_fd >= 0);

    const char *out_dir_path = argc > 2 ? argv[2] : ".";
    int out_dir_fd = open(out_dir_path, O_RDONLY | O_DIRECTORY);
    expect(out_dir_fd >= 0);

    process_directory(in_dir_fd, out_dir_fd);

    close(in_dir_fd);
    close(out_dir_fd);

    return 0;
}
