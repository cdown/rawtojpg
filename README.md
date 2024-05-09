# rawtojpg

rawtojpg provides a much faster way to extract embedded JPEGs from RAW files
than exiftool's `-JpgFromRaw`. In a directory with ~1200 files, rawtojpg
extracts JPEGs about 82 times faster than exiftool:

    % rm -rf ~/rtj && mkdir -p ~/rtj/{rawtojpg, exiftool}
    % sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'
    % \time -v rawtojpg /mnt/sdcard/DCIM/101MSDCF ~/rtj/rawtojpg >/dev/null
            User time (seconds): 0.18
            System time (seconds): 0.43
            Elapsed (wall clock) time (h:mm:ss or m:ss): 0:00.90

    % sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'
    % \time -v exiftool -b -JpgFromRaw -ext arw -r /mnt/sdcard/DCIM/101MSDCF -w ~/rtj/exiftool/%f.jpg >/dev/null
            User time (seconds): 25.89
            System time (seconds): 8.95
            Elapsed (wall clock) time (h:mm:ss or m:ss): 1:14.77

This is much faster because, compared to exiftool:

1. rawtojpg uses fadvise/madvise to avoid reading too much of the large RAW
   file unnecessarily due to readahead;
2. rawtojpg can process multiple files concurrently;
3. rawtojpg can process reading and writing concurrently.
