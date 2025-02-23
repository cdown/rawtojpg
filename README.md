# rawtojpg

rawtojpg provides a much faster way to extract embedded JPEGs from RAW files
than exiftool's `-JpgFromRaw`. In a directory with 4000 files, rawtojpg
extracts JPEGs about 15 times faster than exiftool:

    % rm -rf ~/rtj && mkdir -p ~/rtj/{rawtojpg, exiftool}
    % sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'
    % \time -v rawtojpg /mnt/sdcard/DCIM/101MSDCF ~/rtj/rawtojpg >/dev/null
        User time (seconds): 2.18
        System time (seconds): 20.99
        Elapsed (wall clock) time (h:mm:ss or m:ss): 0:29.72
        File system inputs: 22956551
        File system outputs: 22855504


    % sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'
    % \time -v exiftool -b -JpgFromRaw -ext arw -r /mnt/sdcard/DCIM/101MSDCF -w ~/rtj/exiftool/%f.jpg >/dev/null
        User time (seconds): 113.21
        System time (seconds): 58.15
        Elapsed (wall clock) time (h:mm:ss or m:ss): 7:52.87
        File system inputs: 316175244
        File system outputs: 22864400

The total size of the output JPEGs is 11.5GiB, so in terms of throughput,
rawtojpg does ~386MiB/s, and exiftool does ~24MiB/s.

The key reason rawtojpg is so much faster is because it very carefully avoids
overreading into the entire RAW file. exiftool does not do that and suffers
quite greatly in (useful) throughput as a result. This is achieved through
judicious use of `madvise` (and similar strategies on other platforms).

Other than that, rawtojpg also processes multiple files concurrently, which can
help a lot on faster devices like CFexpress cards.
