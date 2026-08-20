#ifndef MTHOLDEM_H
#define MTHOLDEM_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MTH_ABI_VERSION_MAJOR 0u
#define MTH_ABI_VERSION_MINOR 2u
#define MTH_ABI_VERSION_PATCH 0u

#define MTH_IDENTITY_SEED_BYTES 32u
#define MTH_IDENTITY_PUBLIC_KEY_BYTES 32u

#define MTH_ERROR_SUCCESS 0
#define MTH_ERROR_NULL_POINTER 1
#define MTH_ERROR_BUFFER_TOO_SMALL 2
#define MTH_ERROR_INVALID_ARGUMENT 3
#define MTH_ERROR_NO_EFFECT 4
#define MTH_ERROR_CONFIGURATION 10
#define MTH_ERROR_CODEC 11
#define MTH_ERROR_AUTHENTICATION 12
#define MTH_ERROR_CRYPTOGRAPHY 13
#define MTH_ERROR_RULES 14
#define MTH_ERROR_PROTOCOL 15
#define MTH_ERROR_RECOVERY 16
#define MTH_ERROR_INTERNAL 255

#define MTH_ACTION_FOLD 1u
#define MTH_ACTION_CHECK 2u
#define MTH_ACTION_CALL 3u
#define MTH_ACTION_RAISE_TO 4u
#define MTH_ACTION_ALL_IN 5u

#define MTH_EFFECT_BROADCAST_PUBLIC 1u
#define MTH_EFFECT_SEND_PRIVATE 2u
#define MTH_EFFECT_PERSIST 3u
#define MTH_EFFECT_REQUEST_ACTION 4u
#define MTH_EFFECT_SUBMIT_RECOVERY 5u
#define MTH_EFFECT_PUBLISH_SHOWDOWN_BUNDLE 6u
#define MTH_EFFECT_FINISHED 7u

#define MTH_PEER_EVENT_HAND_STARTED 1u
#define MTH_PEER_EVENT_LOCAL_HOLE_CARDS_OPENED 2u
#define MTH_PEER_EVENT_BETTING_ACTION_ACCEPTED 3u
#define MTH_PEER_EVENT_BOARD_STREET_OPENED 4u
#define MTH_PEER_EVENT_SHOWDOWN_HAND_OPENED 5u
#define MTH_PEER_EVENT_HAND_FINISHED 6u

#define MTH_PHASE_KEY_SETUP 1u
#define MTH_PHASE_SHUFFLE 2u
#define MTH_PHASE_PRIVATE_REVEAL 3u
#define MTH_PHASE_HOLE_CARDS_READY 4u
#define MTH_PHASE_BETTING 5u
#define MTH_PHASE_BOARD_REVEAL 6u
#define MTH_PHASE_SHOWDOWN 7u
#define MTH_PHASE_COMPLETE 8u

#define MTH_TABLE_BACKEND_PEER 1u
#define MTH_TABLE_BACKEND_RULES_ONLY 2u

#define MTH_TABLE_EVENT_HAND_STARTED 1u
#define MTH_TABLE_EVENT_HOLE_CARDS 2u
#define MTH_TABLE_EVENT_ACTION_REQUEST 3u
#define MTH_TABLE_EVENT_ACTION_TAKEN 4u
#define MTH_TABLE_EVENT_BOARD_OPENED 5u
#define MTH_TABLE_EVENT_SHOWDOWN 6u
#define MTH_TABLE_EVENT_HAND_FINISHED 7u
#define MTH_TABLE_EVENT_SESSION_FINISHED 8u

#define MTH_BOT_PRESET_NIT 1u
#define MTH_BOT_PRESET_BALANCED 2u
#define MTH_BOT_PRESET_LOOSE_PASSIVE 3u
#define MTH_BOT_PRESET_LAG 4u
#define MTH_BOT_PRESET_MANIAC 5u

typedef struct MthPeerSession MthPeerSession;
typedef struct MthTableSession MthTableSession;

typedef struct MthSessionConfig {
    const uint8_t *session_id_ptr;
    const uint8_t *roster_public_keys_ptr;
    size_t player_count;
    uint64_t small_blind_units;
    uint64_t big_blind_units;
    uint64_t buy_in_units;
    uint64_t maximum_hand_loss_units;
    uint64_t fault_compensation_units;
    uint64_t security_deposit_units;
    uint64_t timeout_millis;
} MthSessionConfig;

typedef struct MthTableConfig {
    uint32_t backend;
    size_t player_count;
    bool has_human_seat;
    uint8_t human_seat;
    uint32_t bot_preset;
    uint64_t small_blind_units;
    uint64_t big_blind_units;
    uint64_t starting_stack_units;
    bool has_hand_limit;
    uint64_t hand_limit;
    uint64_t seed;
    /* When true, seat_presets_ptr must point at player_count MTH_BOT_PRESET_*
     * codes in seat order; they are copied during construction only. */
    bool has_seat_presets;
    const uint32_t *seat_presets_ptr;
    /* When true, blind_levels_ptr must point at blind_level_count * 2 chip
     * units as (small, big) pairs in level order; they are copied during
     * construction only. Level 0 must equal small_blind_units/big_blind_units,
     * every level must satisfy 0 < small < big, no big blind may exceed the
     * chips in play, and hands_per_blind_level must be at least 1. */
    bool has_blind_schedule;
    const uint64_t *blind_levels_ptr;
    size_t blind_level_count;
    uint64_t hands_per_blind_level;
} MthTableConfig;

typedef struct MthBuffer {
    uint8_t *ptr;
    size_t len;
    size_t cap;
} MthBuffer;

typedef struct MthEffect {
    uint32_t kind;
    bool has_seat;
    uint8_t seat;
    bool has_legal_actions;
    uint64_t to_call_units;
    bool has_minimum_raise_to;
    uint64_t minimum_raise_to_units;
    uint64_t maximum_raise_to_units;
    bool raise_reopened;
    bool can_check;
    bool has_action_context;
    uint8_t button_seat;
    uint64_t pot_units;
    uint64_t actor_stack_units;
    uint64_t actor_street_contributed_units;
    bool has_effective_stack;
    uint64_t effective_stack_units;
    uint8_t live_opponents;
    uint8_t matched_bet_opponents;
    uint64_t big_blind_units;
    uint64_t current_bet_units;
    bool has_transcript_anchor;
    uint64_t next_sequence;
    uint8_t transcript_digest[32];
    MthBuffer artifact;
    MthBuffer checkpoint;
    MthBuffer final_stacks;
    bool has_finished;
    uint64_t finished_hand;
} MthEffect;

typedef struct MthPeerEvent {
    uint32_t kind;
    uint64_t hand;
    uint32_t phase;
    uint8_t local_seat;
    uint8_t button_seat;
    bool has_street;
    uint32_t street;
    bool has_next_actor;
    uint8_t next_actor;
    uint64_t small_blind_units;
    uint64_t big_blind_units;
    uint64_t pot_units;
    bool has_accepted_action;
    uint8_t action_actor;
    uint32_t action_street;
    uint32_t action_kind;
    uint64_t raise_to_units;
    bool has_opened_street;
    uint32_t opened_street;
    bool has_showdown_seat;
    uint8_t showdown_seat;
    /* Current stacks as little-endian u64 chip units in seat order. */
    MthBuffer stacks;
    /* Opened board cards as standard card indices. */
    MthBuffer board_cards;
    /* Only the local peer's two private cards as standard card indices. */
    MthBuffer local_hole_cards;
    /* Verified rows: seat, first card, second card, category. */
    MthBuffer showdown_rows;
    /* Final stacks as little-endian u64 chip units after completion. */
    MthBuffer final_stacks;
} MthPeerEvent;

typedef struct MthTableEvent {
    uint32_t kind;
    uint32_t backend;
    uint64_t hand;
    bool has_button;
    uint8_t button_seat;
    bool has_actor;
    uint8_t actor;
    uint32_t street;
    bool has_legal_actions;
    uint64_t to_call_units;
    bool has_minimum_raise_to;
    uint64_t minimum_raise_to_units;
    uint64_t maximum_raise_to_units;
    bool raise_reopened;
    bool can_check;
    bool has_action_context;
    uint64_t pot_units;
    uint64_t actor_stack_units;
    uint64_t actor_street_contributed_units;
    bool has_effective_stack;
    uint64_t effective_stack_units;
    uint8_t live_opponents;
    uint8_t matched_bet_opponents;
    uint64_t big_blind_units;
    uint64_t current_bet_units;
    bool has_action;
    uint32_t action_kind;
    uint64_t raise_to_units;
    /* Post-action totals reported with MTH_TABLE_EVENT_ACTION_TAKEN. */
    bool has_post_action_totals;
    uint64_t pot_after_units;
    uint64_t actor_stack_after_units;
    /* Posted blinds reported with MTH_TABLE_EVENT_HAND_STARTED; the big blind
     * reuses big_blind_units above. */
    bool has_blinds;
    uint64_t small_blind_units;
    MthBuffer board_cards;
    MthBuffer hole_cards;
    MthBuffer starting_stacks;
    MthBuffer final_stacks;
    MthBuffer showdown_rows;
    uint64_t completed_hands;
} MthTableEvent;

typedef struct MthPeerStatus {
    uint32_t phase;
    uint8_t local_seat;
    bool has_next_public_sender;
    uint8_t next_public_sender;
    uint64_t next_sequence;
    uint8_t transcript_digest[32];
} MthPeerStatus;

typedef struct MthAbiLayout {
    uint32_t version_major;
    uint32_t version_minor;
    uint32_t version_patch;
    size_t session_config_size;
    size_t table_config_size;
    size_t buffer_size;
    size_t effect_size;
    size_t table_event_size;
    size_t peer_status_size;
    size_t abi_layout_size;
    /* ABI 0.2 extension; preceding fields preserve the complete ABI 0.1 prefix. */
    size_t peer_event_size;
} MthAbiLayout;

uint32_t mth_abi_version_major(void);
uint32_t mth_abi_version_minor(void);
uint32_t mth_abi_version_patch(void);

int32_t mth_identity_public_from_seed(
    const uint8_t *seed_ptr,
    uint8_t *out_public_key_ptr);

/* ABI 0.2 layout operations are versioned and size-qualified. They do not
 * write or read the layout when layout_size is smaller than sizeof(MthAbiLayout). */
int32_t mth_abi_layout_v2(
    MthAbiLayout *out_layout,
    size_t out_layout_size);
int32_t mth_abi_check_layout_v2(
    const MthAbiLayout *layout,
    size_t layout_size);

void mth_buffer_free(MthBuffer buffer);
void mth_effect_free(MthEffect effect);
void mth_peer_event_free(MthPeerEvent event);
void mth_table_event_free(MthTableEvent event);

int32_t mth_table_session_new(
    MthTableConfig config,
    MthTableSession **out_handle);

void mth_table_session_free(MthTableSession *handle);
int32_t mth_table_start_next_hand(MthTableSession *handle);

int32_t mth_table_submit_action(
    MthTableSession *handle,
    uint32_t action_kind,
    uint64_t raise_to_units);

int32_t mth_table_next_event(
    MthTableSession *handle,
    MthTableEvent *out_event);

int32_t mth_table_session_last_error(
    MthTableSession *handle,
    uint8_t *out_ptr,
    size_t out_len,
    size_t *out_required_len);

int32_t mth_peer_session_new(
    MthSessionConfig config,
    const uint8_t *local_identity_seed_ptr,
    uint8_t local_seat,
    uint64_t hand,
    MthPeerSession **out_handle);

void mth_peer_session_free(MthPeerSession *handle);
int32_t mth_peer_session_progress(MthPeerSession *handle);

int32_t mth_peer_session_apply_public(
    MthPeerSession *handle,
    const uint8_t *bytes_ptr,
    size_t bytes_len);

int32_t mth_peer_session_apply_private(
    MthPeerSession *handle,
    const uint8_t *bytes_ptr,
    size_t bytes_len);

int32_t mth_peer_session_apply_checkpoint(
    MthPeerSession *handle,
    const uint8_t *bytes_ptr,
    size_t bytes_len);

int32_t mth_peer_session_announce_checkpoint(
    MthPeerSession *handle,
    const uint8_t *bytes_ptr,
    size_t bytes_len);

int32_t mth_peer_session_apply_showdown(
    MthPeerSession *handle,
    const uint8_t *bytes_ptr,
    size_t bytes_len);

int32_t mth_peer_session_submit_action(
    MthPeerSession *handle,
    uint32_t action_kind,
    uint64_t raise_to_units);

int32_t mth_peer_session_checkpoint_state(
    MthPeerSession *handle,
    MthBuffer *out_buffer);

int32_t mth_peer_session_recovery_bundle(
    MthPeerSession *handle,
    MthBuffer *out_buffer);

int32_t mth_peer_session_request_recovery(MthPeerSession *handle);

int32_t mth_peer_session_status(
    MthPeerSession *handle,
    MthPeerStatus *out_status);

int32_t mth_peer_session_next_effect(
    MthPeerSession *handle,
    MthEffect *out_effect);

int32_t mth_peer_session_next_presentation_event(
    MthPeerSession *handle,
    MthPeerEvent *out_event);

int32_t mth_peer_session_last_error(
    MthPeerSession *handle,
    uint8_t *out_ptr,
    size_t out_len,
    size_t *out_required_len);

int32_t mth_last_error_message(
    uint8_t *out_ptr,
    size_t out_len,
    size_t *out_required_len);

#ifdef __cplusplus
}
#endif

#endif
