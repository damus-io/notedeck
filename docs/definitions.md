# Notedeck definitions
Notedeck introduces many niche and novel concepts.
It's important to have a shared understanding on what they all mean.
This document will serve as ground-truth for defining concepts used in Notedeck.

## Account
a `Keypair` which is saved to disk. 
The `Keypair` consists of a required public key and an optional secret key.

## Account Manager
responsible for adding, removing, querying for, and creating new accounts.
It has access to all accounts saved to disk.

## Column
A column has access to one account, or None.
It presents data vertically, in a column.
There are many different types of columns, each of which present data differently.

The following is an exhaustive list of column types:
- Home
- Notifications
- Direct Messages
- Global Feed
- My Profile
- Relays
- Followers List
- Follows List
- Bookmarks
- Follower profile
- Follows profile

## Deck
A deck presents an ordered collection of columns to the user.
Columns are presented from left to right in the order of the collection.
Each column takes up the full vertical height of the app.
Columns can be resized in the horizontal direction.
The current deck takes up the whole surface area of the app, besides the side panel.
The deck has one account associated to it.
Each column in the deck has the following choice of accessing a keypair: it can use the deck's account, a different, (public only) keypair, or None.

## Deck Configuration
A deck configuration is all of the deck's settings encoded into a parameterized replaceable event nostr note.
A deck's configuration is read in order to initialize a deck.

## Account Switching
The user can switch between accounts.

### Deck switching
Once the user has selected an account, they can choose which deck they want to present on the screen.
Each account can have any number (0, 1, 2, ...+) of decks associated with it.
