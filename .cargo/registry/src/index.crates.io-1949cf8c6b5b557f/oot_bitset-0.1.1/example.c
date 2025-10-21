

#include <stdio.h>
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>

#include "oot_bitset.h"

enum game_events {
	FLAG_MET_RUTO_FIRST_TIME,               // 0x00
	FLAG_PLAYED_SONG_FOR_ADULT_MALON,       // 0x01
	FLAG_TALKED_TO_ADULT_MALON_AFTER_SONG,  // 0x02
	FLAG_TALKED_TO_MALON_FIRST_TIME,        // 0x03
	FLAG_TALKED_TO_TALON_IN_RANCH_HOUSE,    // 0x04
	FLAG_TOLD_EPONA_IS_SCARED,              // 0x05
	FLAG_HAS_DEKU_STICK_UPGRADE,            // 0x06
	FLAG_HAS_DEKU_NUT_UPGRADE,              // 0x07

	FLAG_SAW_BOB   = 0x10,
	FLAG_SAW_ALICE = 0x1A,
};

static int print_u16_bits(uint16_t word)
{
	int i;
	uint16_t mask;
	for (i = 0; i < 16; i++) {
		mask = 1 << (15-i);
		printf("%s", (mask & word) == mask ? "1" : "0");
	}

	printf("\n");
}

static int print_bits(uint16_t *words, int count)
{
	int i;
	printf("word ");
	for (i = 0; i < 16; i++) {
		printf("%X", 15-i);
	}
	printf("\n---------------------\n");

	for (i = 0; i < count; i++) {
		printf("0x%01x_ ", i);
		print_u16_bits(words[i]);
	}
}

int main() {
	// we have
	uint16_t flags[2] = {0};
	bool is_set = 0;

	is_set = bitset_get(flags, FLAG_TALKED_TO_ADULT_MALON_AFTER_SONG);
	assert(is_set == 0);

	bitset_set(flags, FLAG_TALKED_TO_ADULT_MALON_AFTER_SONG);
	is_set = bitset_get(flags, FLAG_TALKED_TO_ADULT_MALON_AFTER_SONG);
	assert(is_set);

	// 3rd bit set
	assert(bitset_word(flags, FLAG_TALKED_TO_ADULT_MALON_AFTER_SONG) == 4);
	assert(bitset_index(FLAG_TALKED_TO_ADULT_MALON_AFTER_SONG) == 0);

	// 2nd word
	assert(bitset_index(FLAG_SAW_BOB) == 1);

	bitset_set(flags, FLAG_SAW_BOB);
	assert(bitset_get(flags, FLAG_SAW_BOB));

	bitset_clear(flags, FLAG_SAW_BOB);
	assert(bitset_get(flags, FLAG_SAW_BOB) == 0);
	bitset_set(flags, FLAG_SAW_BOB);

	bitset_set(flags, FLAG_SAW_ALICE);

	print_bits(flags, 2);

	return 1;
}
