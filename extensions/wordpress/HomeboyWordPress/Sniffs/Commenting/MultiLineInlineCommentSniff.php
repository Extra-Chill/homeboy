<?php
/**
 * Enforces WordPress Inline Documentation Standards section 5.2:
 *
 * 1. Multi-line comments must use slash-star block style, not consecutive `//` lines.
 * 2. Multi-line prose comments must start with a single asterisk, not a double
 *    asterisk (which is reserved for DocBlocks).
 *
 * @link https://developer.wordpress.org/coding-standards/inline-documentation-standards/php/#5-2-multi-line-comments
 *
 * @package HomeboyWordPress
 */

namespace HomeboyWordPress\Sniffs\Commenting;

use PHP_CodeSniffer\Files\File;
use PHP_CodeSniffer\Sniffs\Sniff;
use PHP_CodeSniffer\Util\Tokens;

/**
 * Custom sniff implementing WordPress inline-docs section 5.2 rules.
 *
 * Two independent detections live under this sniff so they can be excluded
 * separately via `<exclude name="...ConsecutiveSingleLine"/>` etc.:
 *
 *  - ConsecutiveSingleLine     2+ adjacent `//` lines that should be a block comment.
 *  - DoubleAsteriskNonDocBlock A double-asterisk block used for prose, not a declaration.
 */
class MultiLineInlineCommentSniff implements Sniff {

	/**
	 * Minimum run length to trigger Detection A. Default 2.
	 *
	 * @var int
	 */
	public $minimumRunLength = 2;

	/**
	 * Tokens that, when they immediately follow a `/**` block, mean the block is
	 * a legitimate DocBlock (so Detection B should NOT fire).
	 *
	 * Mirrors the lookahead used by Squiz.Commenting.BlockComment plus the WP
	 * handbook's list of declaration contexts (function, class, method, property,
	 * const, require/include).
	 *
	 * @var array<int|string,bool>
	 */
	private $docBlockContextTokens = array();

	/**
	 * Returns the tokens this sniff registers for.
	 *
	 * @return array<int|string>
	 */
	public function register() {
		// Build the DocBlock-context lookup once.
		$this->docBlockContextTokens                   = Tokens::$scopeModifiers;
		$this->docBlockContextTokens[ T_FUNCTION ]     = true;
		$this->docBlockContextTokens[ T_CLASS ]        = true;
		$this->docBlockContextTokens[ T_INTERFACE ]    = true;
		$this->docBlockContextTokens[ T_TRAIT ]        = true;
		$this->docBlockContextTokens[ T_ENUM ]         = true;
		$this->docBlockContextTokens[ T_FINAL ]        = true;
		$this->docBlockContextTokens[ T_STATIC ]       = true;
		$this->docBlockContextTokens[ T_ABSTRACT ]     = true;
		$this->docBlockContextTokens[ T_CONST ]        = true;
		$this->docBlockContextTokens[ T_VAR ]          = true;
		$this->docBlockContextTokens[ T_REQUIRE ]      = true;
		$this->docBlockContextTokens[ T_REQUIRE_ONCE ] = true;
		$this->docBlockContextTokens[ T_INCLUDE ]      = true;
		$this->docBlockContextTokens[ T_INCLUDE_ONCE ] = true;
		if ( defined( 'T_READONLY' ) ) {
			$this->docBlockContextTokens[ T_READONLY ] = true;
		}

		return array(
			T_COMMENT,
			T_DOC_COMMENT_OPEN_TAG,
		);
	}

	/**
	 * Dispatches to Detection A or B based on the token kind.
	 *
	 * @param File $phpcsFile The file being scanned.
	 * @param int  $stackPtr  The position of the current token in the stack.
	 *
	 * @return int|void
	 */
	public function process( File $phpcsFile, $stackPtr ) {
		$tokens = $phpcsFile->getTokens();

		if ( T_DOC_COMMENT_OPEN_TAG === $tokens[ $stackPtr ]['code'] ) {
			$this->processDoubleAsterisk( $phpcsFile, $stackPtr );
			return;
		}

		/*
		 * T_COMMENT — only `//` runs are interesting here. Block comments also
		 * arrive as T_COMMENT but are already the desired output style.
		 */
		$content = $tokens[ $stackPtr ]['content'];
		if ( substr( $content, 0, 2 ) !== '//' ) {
			return;
		}

		return $this->processConsecutiveSingleLine( $phpcsFile, $stackPtr );
	}

	/**
	 * Detection A — flag a run of 2+ adjacent `//` lines.
	 *
	 * Returns the position to skip to so PHPCS doesn't re-process the same run.
	 *
	 * @param File $phpcsFile File being scanned.
	 * @param int  $stackPtr  Position of the first `//` comment in the run.
	 *
	 * @return int
	 */
	private function processConsecutiveSingleLine( File $phpcsFile, $stackPtr ) {
		$tokens = $phpcsFile->getTokens();

		// Skip end-of-line trailing comments (`$x = 1; // explanation`).
		if ( $this->isTrailingComment( $phpcsFile, $stackPtr ) ) {
			return ( $stackPtr + 1 );
		}

		// Skip annotation lines that happen to start the run.
		if ( $this->isAnnotationLine( $tokens[ $stackPtr ]['content'] ) ) {
			return ( $stackPtr + 1 );
		}

		// Walk forward collecting consecutive `//` tokens on adjacent lines.
		$run        = array( $stackPtr );
		$lastLine   = $tokens[ $stackPtr ]['line'];
		$ptr        = $stackPtr;
		$tokenCount = count( $tokens );

		for ( $ptr = ( $stackPtr + 1 ); $ptr < $tokenCount; $ptr++ ) {
			$code = $tokens[ $ptr ]['code'];

			if ( T_WHITESPACE === $code ) {
				continue;
			}

			if ( T_COMMENT !== $code ) {
				break;
			}

			$candidateContent = $tokens[ $ptr ]['content'];
			if ( substr( $candidateContent, 0, 2 ) !== '//' ) {
				break;
			}

			if ( ( $tokens[ $ptr ]['line'] - 1 ) !== $lastLine ) {
				break;
			}

			if ( $this->isTrailingComment( $phpcsFile, $ptr ) ) {
				break;
			}

			if ( $this->isAnnotationLine( $candidateContent ) ) {
				break;
			}

			$run[]    = $ptr;
			$lastLine = $tokens[ $ptr ]['line'];
		}

		if ( count( $run ) < $this->minimumRunLength ) {
			return ( end( $run ) + 1 );
		}

		/*
		 * Heuristic: leave commented-out code alone — Squiz.PHP.CommentedOutCode
		 * already covers it. Only skip if EVERY line in the run looks like code.
		 */
		if ( $this->runLooksLikeCommentedOutCode( $tokens, $run ) ) {
			return ( end( $run ) + 1 );
		}

		$error = 'Multi-line comments should use /* */ block style per WP inline docs §5.2; found %d consecutive // lines.';
		$data  = array( count( $run ) );
		$fix   = $phpcsFile->addFixableError( $error, $stackPtr, 'ConsecutiveSingleLine', $data );

		if ( true === $fix ) {
			$this->fixConsecutiveSingleLine( $phpcsFile, $run );
		}

		return ( end( $run ) + 1 );
	}

	/**
	 * Detection B — flag `/**` blocks that aren't preceding a declaration.
	 *
	 * Lookahead pattern cribbed from Squiz.Commenting.BlockCommentSniff::process().
	 *
	 * @param File $phpcsFile File being scanned.
	 * @param int  $stackPtr  Position of the T_DOC_COMMENT_OPEN_TAG.
	 *
	 * @return void
	 */
	private function processDoubleAsterisk( File $phpcsFile, $stackPtr ) {
		$tokens = $phpcsFile->getTokens();

		// Walk forward past whitespace, comments, and attribute groups.
		$nextToken = $stackPtr;
		do {
			$nextToken = $phpcsFile->findNext( Tokens::$emptyTokens, ( $nextToken + 1 ), null, true );
			if ( false === $nextToken ) {
				break;
			}

			if ( defined( 'T_ATTRIBUTE' ) && T_ATTRIBUTE === $tokens[ $nextToken ]['code'] ) {
				$nextToken = $tokens[ $nextToken ]['attribute_closer'];
				continue;
			}

			break;
		} while ( true );

		if ( false !== $nextToken && isset( $this->docBlockContextTokens[ $tokens[ $nextToken ]['code'] ] ) === true ) {
			// Legitimate DocBlock — leave it alone.
			return;
		}

		// File-level DocBlock immediately after `<?php` is a legitimate use too.
		$prevToken = $phpcsFile->findPrevious( Tokens::$emptyTokens, ( $stackPtr - 1 ), null, true );
		if ( false !== $prevToken && T_OPEN_TAG === $tokens[ $prevToken ]['code'] ) {
			return;
		}

		$error = 'Multi-line prose comments must use /* (single asterisk), not /** (reserved for DocBlocks). See WP inline docs §5.2.';
		$fix   = $phpcsFile->addFixableError( $error, $stackPtr, 'DoubleAsteriskNonDocBlock' );

		if ( true === $fix ) {
			$phpcsFile->fixer->replaceToken( $stackPtr, '/*' );
		}
	}

	/**
	 * Whether the comment at $stackPtr sits on the same line as preceding code.
	 *
	 * @param File $phpcsFile File being scanned.
	 * @param int  $stackPtr  Token position.
	 *
	 * @return bool
	 */
	private function isTrailingComment( File $phpcsFile, $stackPtr ) {
		$tokens = $phpcsFile->getTokens();
		$line   = $tokens[ $stackPtr ]['line'];

		for ( $i = ( $stackPtr - 1 ); $i >= 0; $i-- ) {
			if ( $tokens[ $i ]['line'] !== $line ) {
				return false;
			}

			if ( T_WHITESPACE === $tokens[ $i ]['code'] ) {
				continue;
			}

			if ( T_OPEN_TAG === $tokens[ $i ]['code'] ) {
				return false;
			}

			// Any other token on the same line means this is a trailing comment.
			return true;
		}

		return false;
	}

	/**
	 * Whether a comment line is a tooling annotation that must not be rewritten.
	 *
	 * @param string $content Raw comment token content (still has leading `//`).
	 *
	 * @return bool
	 */
	private function isAnnotationLine( $content ) {
		$trimmed = ltrim( $content, '/' );
		$trimmed = ltrim( $trimmed );

		if ( '' === $trimmed ) {
			return false;
		}

		$lower = strtolower( $trimmed );

		if ( strpos( $lower, 'phpcs:' ) === 0 ) {
			return true;
		}

		if ( strpos( $lower, 'translators:' ) === 0 ) {
			return true;
		}

		/*
		 * `//phpstan-ignore-line`, `//psalm-suppress`, etc. — no space after `//`.
		 * Match against the raw token content, not the trimmed body.
		 */
		$rawLower = strtolower( $content );
		if ( strpos( $rawLower, '//phpstan-' ) === 0 || strpos( $rawLower, '// phpstan-' ) === 0 ) {
			return true;
		}

		if ( strpos( $rawLower, '//psalm-' ) === 0 || strpos( $rawLower, '// psalm-' ) === 0 ) {
			return true;
		}

		return false;
	}

	/**
	 * Heuristic: every line in the run looks like commented-out PHP code.
	 *
	 * Defers to Squiz.PHP.CommentedOutCode for the actual finding.
	 *
	 * @param array<int,array<string,mixed>> $tokens The full token list.
	 * @param array<int,int>                 $run    Token positions in the run.
	 *
	 * @return bool
	 */
	private function runLooksLikeCommentedOutCode( array $tokens, array $run ) {
		$signals = array( ';', '{', '}', '<?php', '->', '::', '=>' );

		foreach ( $run as $ptr ) {
			$body = ltrim( $tokens[ $ptr ]['content'], '/' );
			$body = trim( $body );

			$matched = false;
			foreach ( $signals as $signal ) {
				if ( strpos( $body, $signal ) !== false ) {
					$matched = true;
					break;
				}
			}

			if ( false === $matched ) {
				return false;
			}
		}

		return true;
	}

	/**
	 * Replaces a run of `//` comments with a `/* ... *\/` block, preserving indent.
	 *
	 * @param File           $phpcsFile File being fixed.
	 * @param array<int,int> $run       Token positions of the comments in the run.
	 *
	 * @return void
	 */
	private function fixConsecutiveSingleLine( File $phpcsFile, array $run ) {
		$tokens = $phpcsFile->getTokens();
		$eol    = $phpcsFile->eolChar;

		$firstPtr = $run[0];
		$indent   = '';
		if ( $firstPtr > 0 && T_WHITESPACE === $tokens[ $firstPtr - 1 ]['code'] ) {
			$prev = $tokens[ $firstPtr - 1 ]['content'];
			// `content` for an indent whitespace token is the run of leading spaces/tabs.
			$lastNl = strrpos( $prev, "\n" );
			if ( false !== $lastNl ) {
				$indent = substr( $prev, $lastNl + 1 );
			} else {
				$indent = $prev;
			}
		}

		$bodyLines = array();
		foreach ( $run as $ptr ) {
			$line = $tokens[ $ptr ]['content'];
			// Strip trailing newline; we'll re-emit with $eol.
			$line = rtrim( $line, "\r\n" );
			// Strip the leading `//` and one optional space.
			$line        = preg_replace( '#^//[ \t]?#', '', $line );
			$bodyLines[] = $line;
		}

		$replacement = '/*' . $eol;
		foreach ( $bodyLines as $line ) {
			$replacement .= $indent . ' * ' . $line . $eol;
		}
		$replacement .= $indent . ' */' . $eol;

		$phpcsFile->fixer->beginChangeset();
		$phpcsFile->fixer->replaceToken( $firstPtr, $replacement );
		for ( $i = 1, $n = count( $run ); $i < $n; $i++ ) {
			$phpcsFile->fixer->replaceToken( $run[ $i ], '' );
		}
		$phpcsFile->fixer->endChangeset();
	}
}
