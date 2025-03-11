package com.damus.notedeck;

import android.os.Bundle;
import android.util.Log;
import android.view.MotionEvent;
import android.view.View;
import android.view.ViewGroup;

import androidx.core.graphics.Insets;
import androidx.core.view.DisplayCutoutCompat;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsCompat;
import androidx.core.view.WindowInsetsControllerCompat;

import com.google.androidgamesdk.GameActivity;

public class MainActivity extends GameActivity {
  static {
    System.loadLibrary("notedeck_chrome");
  }

  private native void nativeOnKeyboardHeightChanged(int height);
  private KeyboardHeightHelper keyboardHelper;
  
  @Override
  protected void onCreate(Bundle savedInstanceState) {
      // Shrink view so it does not get covered by insets.

      setupInsets();
      //setupFullscreen()
      keyboardHelper = new KeyboardHeightHelper(this);
      
      super.onCreate(savedInstanceState);
  }

  private void setupFullscreen() {
      WindowCompat.setDecorFitsSystemWindows(getWindow(), false);
      
      WindowInsetsControllerCompat controller =
          WindowCompat.getInsetsController(getWindow(), getWindow().getDecorView());
      if (controller != null) {
          controller.setSystemBarsBehavior(
      	WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
          );
          controller.hide(WindowInsetsCompat.Type.systemBars());
      }

      //focus(getContent())
  }

  // not sure if this does anything
  private void focus(View content) {
      content.setFocusable(true);
      content.setFocusableInTouchMode(true);
      content.requestFocus();
  }

  private View getContent() {
	return getWindow().getDecorView().findViewById(android.R.id.content);
  }

  private void setupInsets() {
      View content = getContent();
      ViewCompat.setOnApplyWindowInsetsListener(content, (v, windowInsets) -> {
        Insets insets = windowInsets.getInsets(WindowInsetsCompat.Type.systemBars());

        ViewGroup.MarginLayoutParams mlp = (ViewGroup.MarginLayoutParams) v.getLayoutParams();
        mlp.topMargin = insets.top;
        mlp.leftMargin = insets.left;
        mlp.bottomMargin = insets.bottom;
        mlp.rightMargin = insets.right;
        v.setLayoutParams(mlp);

        return WindowInsetsCompat.CONSUMED;
      });

      WindowCompat.setDecorFitsSystemWindows(getWindow(), true);
  }
  
  @Override
  public void onResume() {
      super.onResume();
      keyboardHelper.start();
  }
  
  @Override
  public void onPause() {
      super.onPause();
      keyboardHelper.stop();
  }
  
  @Override
  public void onDestroy() {
      super.onDestroy();
      keyboardHelper.close();
  }

  @Override
  public boolean onTouchEvent(MotionEvent event) {
      // Offset the location so it fits the view with margins caused by insets.

      int[] location = new int[2];
      findViewById(android.R.id.content).getLocationOnScreen(location);
      event.offsetLocation(-location[0], -location[1]);

      return super.onTouchEvent(event);
  }
}
