CFLAGS := -O2 -Wall -Wextra -Wshadow -Wpointer-arith \
	  -Wcast-align -Wmissing-prototypes -Wstrict-overflow -Wformat=2 \
	  -Wwrite-strings -Warray-bounds -Wstrict-prototypes \
	  -Werror $(CFLAGS)
LDLIBS += -luring
PREFIX ?= /usr/local
bindir := $(PREFIX)/bin
debug_cflags := -D_FORTIFY_SOURCE=2 -fsanitize=leak -fsanitize=address \
	        -fsanitize=undefined -Og -ggdb -fno-omit-frame-pointer \
	        -fstack-protector-strong

bins := arwtojpg

all: $(bins)
arwtojpg: arwtojpg.c
	$(CC) $(CFLAGS) $(CPPFLAGS) $^ $(LDFLAGS) $(LDLIBS) -o $@

debug: all
debug: CFLAGS+=$(debug_cflags)

install: all
	mkdir -p $(DESTDIR)$(bindir)/
	install -pt $(DESTDIR)$(bindir)/ $(bins)

uninstall:
	rm -f $(addprefix $(DESTDIR)$(PREFIX)/bin/,$(bins))

clean:
	rm -f $(bins)

clang_supports_unsafe_buffer_usage := $(shell clang -x c -c /dev/null -o /dev/null -Werror -Wunsafe-buffer-usage > /dev/null 2>&1; echo $$?)
ifeq ($(clang_supports_unsafe_buffer_usage),0)
    extra_clang_flags := -Wno-unsafe-buffer-usage
else
    extra_clang_flags :=
endif

analyse: CFLAGS+=$(debug_cflags)
analyse:
	# -W options here are not clang compatible, so out of generic CFLAGS
	gcc arwtojpg.c -o /dev/null -c \
		-std=gnu99 -Ofast -fwhole-program -Wall -Wextra \
		-Wlogical-op -Wduplicated-cond \
		-fanalyzer $(CFLAGS) $(CPPFLAGS) $(LDFLAGS) $(LDLIBS)
	clang arwtojpg.c -o /dev/null -c -std=gnu99 -Ofast -Weverything \
		-Wno-documentation-unknown-command \
		-Wno-language-extension-token \
		-Wno-disabled-macro-expansion \
		-Wno-padded \
		-Wno-covered-switch-default \
		-Wno-gnu-zero-variadic-macro-arguments \
		-Wno-declaration-after-statement \
		-Wno-cast-qual \
		-Wno-unused-command-line-argument \
		$(extra_clang_flags) \
		$(CFLAGS) $(CPPFLAGS) $(LDFLAGS) $(LDLIBS)
	# cppcheck is a bit dim about unused functions/variables, leave that to
	# clang/GCC
	cppcheck arwtojpg.c --std=c99 --quiet --inline-suppr --force \
		--enable=all --suppress=missingIncludeSystem \
		--suppress=unusedFunction --suppress=unmatchedSuppression \
		--suppress=unreadVariable \
		--max-ctu-depth=32 --error-exitcode=1
	clang-tidy arwtojpg.c --quiet -- -std=gnu99
	clang-format --dry-run --Werror arwtojpg.c

.PHONY: all debug install uninstall clean analyse
