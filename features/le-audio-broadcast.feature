# The first specification written this way. See docs/bdd-evaluation.md for why
# this exists and what it deliberately is not.
#
# Nothing executes this file. It is intent, in the language a person building
# an Auracast device would use, kept beside the tests that cover it so a tool —
# or a reader — can ask what is missing. Each scenario names its test.
#
# Written after the fact, describing what shipped, INCLUDING the three failure
# scenarios nobody thought to write until each became a bug.

Feature: LE Audio Broadcast (Auracast)
  As someone building a broadcast audio device
  I want one source to reach any number of listeners with no connection
  So that a venue can serve headphones it has never met

  Background:
    Given a broadcast source configured for 2 BIS
    And the source is streaming

  # covered by: broadcast_e2e_test::a_receiver_joins_the_big_the_source_created
  Scenario: A listener discovers a broadcast and joins it
    When a listener scans and finds the Broadcast Audio Announcement
    Then it synchronises to the periodic advertising train
    And it reads the BASE from that train
    And it joins the BIG using the BIS indices the BASE named
    And it receives audio on its own BIS handles

  # covered by: broadcast_e2e_test::three_receivers_join_without_the_source_knowing
  # This is the property that makes broadcast broadcast, and it is asserted
  # numerically rather than described: netsim's own counters show the source at
  # tx 2607 / rx 0 and each receiver at rx 2607 / tx 0.
  Scenario: Listeners are invisible to the source
    When three listeners join the same BIG
    Then each receives the same audio
    And the source's state is unchanged
    And the source receives nothing from any listener

  # covered by: tests/interop/auracast_sink.py (Bumble decodes our broadcast)
  # The only evidence about the wire. Both ends of every other test here are
  # this codebase, which cannot disagree with itself.
  Scenario: A foreign receiver understands the published BASE
    When a listener built on a different Bluetooth stack joins
    Then it reports the codec configuration the source published
    And audio encoded for the left BIS arrives on the left channel
    And audio encoded for the right BIS arrives on the right channel

  # --- the scenarios nobody wrote, until each was a bug -------------------

  # covered by: broadcast_e2e_test::a_receiver_that_leaves_the_big_stops_reporting_that_it_is_receiving
  # WAS A BUG. LE BIG Terminate Sync is answered by Command Complete and
  # nothing else; nothing handled it, so a receiver that left kept reporting
  # Receiving forever and held stale handles.
  Scenario: A listener leaves the broadcast
    Given a listener that has joined the BIG
    When it terminates its synchronisation
    Then it stops reporting that it is receiving
    And it holds no stream handles
    And a late packet on those handles is not treated as audio

  # covered by: broadcast_e2e_test::the_source_tearing_down_reaches_every_receiver
  Scenario: The source stops broadcasting while listeners are synchronised
    Given two listeners that have joined the BIG
    When the source terminates the BIG
    Then each listener is told the synchronisation was lost
    And the reason given is that the remote user terminated it
    And each listener may re-acquire when the broadcast returns

  # covered by: broadcast_e2e_test::an_encrypted_source_refuses_a_codeless_listener
  # Honest limit, recorded here because the page states it too: rootcanal does
  # not encrypt BIS payloads, so a listener with the WRONG code still receives
  # plaintext. Only the refusal below is real on this controller.
  Scenario: A listener that was never told the broadcast code refuses to join
    Given an encrypted broadcast source
    When a listener with no broadcast code discovers it
    Then it refuses to synchronise before requesting the stream
    And it receives no audio
    And a listener that does hold the code is unaffected

  # covered by: broadcast_e2e_test::create_big_before_the_train_is_refused
  Scenario: A source that tries to create a BIG before advertising periodically
    Given a source whose periodic advertising train is not running
    When it asks the controller to create the BIG
    Then the controller refuses the command
    And the source does not report that it is streaming

  # --- known gaps, stated rather than omitted -----------------------------
  # A specification that quietly omits what it cannot test is worse than one
  # that says so. These have no covering test and each says why.

  @gap @unprovable-on-this-controller
  Scenario: Audio is unreadable to a listener with the wrong broadcast code
    # rootcanal compares the code but never encrypts the payload, so this
    # cannot be demonstrated here. It needs a controller that encrypts.

  @gap @not-modelled
  Scenario: A listener recovers when the periodic train is briefly lost
    # Nothing in the in-process controller has a clock, so sync timeouts and
    # retransmission are not modelled at all.

  @gap
  Scenario: A listener joins only some of the BISes in a subgroup
    # The routing supports it — fan-out is by BIS index — but BigReceiver
    # always requests every index, so a test would have to hand-write the
    # Create Sync and would prove nothing about the receiver.
